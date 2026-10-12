//! Main control panel: live preview with the settings grouped in tabs.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Align2, Color32, CornerRadius, Layout, Margin, Rect, Sense, Stroke, Ui, pos2, vec2};

use crate::theme::{self, ACCENT, AMBER, CARD, GREEN, LINE, PANEL, RAISED, RED, Slider, TEXT, WEAK, card, hint};
use crate::{App, Controls, LIGHTING, Mode, RESOLUTIONS};

/// Width at which the settings move from below the preview to the side.
const WIDE: f32 = 1100.0;
/// Widest the single column grows.
const COLUMN: f32 = 640.0;
const SIDE_WIDTH: f32 = 420.0;
const TILE_W: f32 = 84.0;
const TILE_GAP: f32 = 8.0;

#[derive(Clone, Copy)]
enum Pick {
    Background,
    Look,
    Sticker,
}

/// A sticker as the service lists it.
#[derive(Clone)]
struct StickerRow {
    id: String,
    kind: String,
    x: f32,
    y: f32,
    size: f32,
    flags: String,
    details: String,
}

impl StickerRow {
    fn parse(line: &str) -> Option<Self> {
        let w: Vec<&str> = line.split_whitespace().collect();
        let [id, kind, x, y, size, flags, details @ ..] = w.as_slice() else {
            return None;
        };
        Some(Self {
            id: (*id).to_owned(),
            kind: (*kind).to_owned(),
            x: x.parse().ok()?,
            y: y.parse().ok()?,
            size: size.parse().ok()?,
            flags: (*flags).to_owned(),
            details: details.join(" "),
        })
    }

    fn label(&self) -> String {
        let what = match self.kind.as_str() {
            "text" => self
                .details
                .split_once(' ')
                .map_or(self.details.as_str(), |(_, t)| t)
                .to_owned(),
            "card" => self.details.split('|').next().unwrap_or("").trim().to_owned(),
            "picture" => Path::new(&self.details)
                .file_name()
                .map_or(String::new(), |n| n.to_string_lossy().into_owned()),
            k => title_case(k),
        };
        match self.kind.as_str() {
            "text" => format!("Text: {what}"),
            "card" => format!("Name card: {what}"),
            "picture" => format!("Picture: {what}"),
            _ => what,
        }
    }
}

/// Colours of a tiny portrait: sky, wall, face, shirt.
type Swatch = [Color32; 4];

const SCENE: [[f32; 3]; 4] = [
    [0.38, 0.62, 0.92],
    [0.30, 0.52, 0.34],
    [0.93, 0.72, 0.58],
    [0.86, 0.30, 0.26],
];

pub struct State {
    /// Camera settings are shown.
    more: bool,
    /// The help page is open.
    help: bool,
    /// Open the help page scrolled to the top.
    help_top: bool,
    /// Background picture switched on, waiting for a file.
    picking_background: bool,
    /// Scroll the chosen look into view once.
    reveal_look: bool,
    /// The look comes from a file the user picks.
    custom_look: bool,
    look_file: String,
    swatches: Vec<Swatch>,
    plain: Swatch,
    /// File chosen in the picker, filled by its thread.
    picked: Arc<Mutex<Option<(Pick, String)>>>,
    can_browse: bool,
    /// Do not reopen the preview before this, while the camera restarts.
    hold: Option<Instant>,
    /// Low power mode only: the preview was asked for. It reads the camera itself, which then turns apps away.
    camera_preview: bool,
    /// The colour picked in the Colours card.
    colour: usize,
    /// The next click on the preview picks a white point.
    pub picking_white: bool,
    /// Saved presets, and the read of the lists they came from.
    presets: Vec<String>,
    lists_seen: u64,
    preset_name: String,
    stickers: Vec<StickerRow>,
    /// The sticker being edited.
    sticker: Option<String>,
    new_kind: String,
    new_text: String,
    new_title: String,
    new_colour: String,
    new_picture: String,
    /// The record button is held down.
    loop_holding: bool,
    #[cfg(unix)]
    cameras: crate::cameras::Card,
    loop_bounce: bool,
}

impl State {
    pub fn new(c: &Controls) -> Self {
        let custom_look = !opal_effects::LOOKS.contains(&c.look.as_str());
        let to_color = |[r, g, b]: [f32; 3]| {
            let u = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            Color32::from_rgb(u(r), u(g), u(b))
        };
        let swatches = opal_effects::LOOKS
            .iter()
            .map(|name| {
                let cube = opal_effects::builtin_look(name);
                SCENE.map(|rgb| to_color(cube.as_ref().map_or(rgb, |c| c.sample(rgb))))
            })
            .collect();
        let can_browse = can_browse();
        Self {
            more: false,
            help: false,
            help_top: false,
            picking_background: false,
            reveal_look: true,
            custom_look,
            look_file: if custom_look { c.look.clone() } else { String::new() },
            swatches,
            plain: SCENE.map(to_color),
            picked: Arc::default(),
            can_browse,
            hold: None,
            camera_preview: false,
            colour: 0,
            picking_white: false,
            presets: Vec::new(),
            lists_seen: 0,
            preset_name: String::new(),
            stickers: Vec::new(),
            sticker: None,
            new_kind: "text".into(),
            new_text: String::new(),
            new_title: String::new(),
            new_colour: "white".into(),
            new_picture: String::new(),
            loop_holding: false,
            #[cfg(unix)]
            cameras: Default::default(),
            loop_bounce: false,
        }
    }

    /// Opens the desktop file picker without blocking the window.
    fn browse(&self, ctx: &egui::Context, pick: Pick) {
        let (title, kind, extensions) = match pick {
            Pick::Background => ("Choose a background picture", "Pictures", &["jpg", "jpeg", "png"][..]),
            Pick::Look => ("Choose a look file", "Look files", &["cube"][..]),
            Pick::Sticker => ("Choose a picture", "Pictures", &["png", "jpg", "jpeg"][..]),
        };
        let mut command = file_dialog(title, kind, extensions);
        let (slot, ctx) = (self.picked.clone(), ctx.clone());
        std::thread::spawn(move || {
            let out = command.output();
            if let Ok(out) = out
                && out.status.success()
            {
                let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                if !path.is_empty() {
                    *slot.lock().unwrap() = Some((pick, path));
                    ctx.request_repaint();
                }
            }
        });
    }
}

/// The desktop's Open dialog for one kind of file, as a command that prints the chosen path: zenity on Linux, the
/// standard dialog through PowerShell on Windows.
pub fn file_dialog(title: &str, kind: &str, extensions: &[&str]) -> Command {
    #[cfg(unix)]
    let command = {
        let patterns: Vec<String> = extensions
            .iter()
            .flat_map(|e| [format!("*.{e}"), format!("*.{}", e.to_uppercase())])
            .collect();
        let mut c = Command::new("zenity");
        c.args(["--file-selection", "--title", title, "--file-filter"])
            .arg(format!("{kind} | {}", patterns.join(" ")));
        c
    };
    #[cfg(windows)]
    let command = {
        // The standard Open dialog, through PowerShell; it prints the chosen file.
        let patterns: Vec<String> = extensions.iter().map(|e| format!("*.{e}")).collect();
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.OpenFileDialog; \
             $d.Title = '{title}'; $d.Filter = '{kind}|{}'; if ($d.ShowDialog() -eq 'OK') {{ $d.FileName }}",
            patterns.join(";")
        );
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-STA", "-Command", &script]);
        // No console window flashing up.
        std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000);
        c
    };
    command
}

/// Whether a file picker can be shown: always on Windows, with zenity on Linux.
pub fn can_browse() -> bool {
    cfg!(windows)
        || std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("zenity").is_file()))
}

fn title_case(name: &str) -> String {
    let mut s = name.to_owned();
    if let Some(f) = s.get_mut(..1) {
        f.make_ascii_uppercase();
    }
    s
}

fn signed(v: i32) -> String {
    if v > 0 { format!("+{v}") } else { v.to_string() }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    {
        let l = app.lists.lock().unwrap();
        if l.serial != app.view.lists_seen {
            app.view.lists_seen = l.serial;
            app.view.presets = l.presets.split_whitespace().map(str::to_owned).collect();
            app.view.stickers = l.stickers.split("; ").filter_map(StickerRow::parse).collect();
        }
    }
    keep_preview(app, &ctx);
    app.update_texture(&ctx);
    let mut cmds = Vec::new();
    if let Some((pick, path)) = app.view.picked.lock().unwrap().take() {
        match pick {
            Pick::Background => {
                app.view.picking_background = false;
                app.c.background = path;
                app.c.background_on = true;
                cmds.push(format!("background {}", app.c.background));
            }
            Pick::Sticker => app.view.new_picture = path,
            Pick::Look => {
                app.view.look_file = path;
                app.view.custom_look = true;
                app.c.look = app.view.look_file.clone();
                app.c.look_on = true;
                cmds.push(app.c.look_cmd());
            }
        }
    }

    if app.view.help {
        help(app, ui);
        return;
    }
    let wide = ui.available_width() >= WIDE;
    if wide {
        egui::Panel::right("settings")
            .exact_size(SIDE_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0, LINE))
                    .inner_margin(Margin {
                        left: 18,
                        right: 6,
                        top: 18,
                        bottom: 0,
                    }),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("settings")
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width() - 12.0);
                        settings(app, ui, &mut cmds);
                        ui.add_space(18.0);
                    });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.inner_margin(Margin::same(24)))
            .show(ui, |ui| {
                // Leave room for the header above and a message below.
                let (above, below) = (50.0, if app.message.is_empty() { 0.0 } else { 70.0 });
                let max = vec2(ui.available_width(), ui.available_height() - above - below);
                let size = theme::fit_16_9(max);
                // Centre the whole group in the window.
                ui.add_space(((ui.available_height() - size.y - above - below) / 2.0).max(0.0));
                header(ui);
                ui.add_space(10.0);
                stage(app, ui, size);
                message(app, ui);
            });
    } else {
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
            // Keep the column readable in a wide window.
            let w = (ui.available_width() - 32.0).min(COLUMN);
            let left = (ui.available_width() - w) / 2.0;
            // The preview stays put; only the settings scroll.
            let size = theme::fit_16_9(vec2(w, (ui.available_height() * 0.42).max(150.0)));
            ui.add_space(16.0);
            column(ui, left, w, |ui| {
                header(ui);
                ui.add_space(10.0);
                stage(app, ui, size);
                message(app, ui);
            });
            ui.add_space(12.0);
            egui::ScrollArea::vertical()
                .id_salt("settings")
                .auto_shrink(false)
                .show(ui, |ui| {
                    column(ui, left, w, |ui| {
                        settings(app, ui, &mut cmds);
                        ui.add_space(16.0);
                    });
                });
        });
    }

    let restart = cmds
        .iter()
        .any(|c| c.starts_with("resolution ") || c.starts_with("power "));
    let presets = cmds
        .iter()
        .any(|c| c.starts_with("preset ") || c.starts_with("sticker "));
    let loaded = cmds.iter().any(|c| c.starts_with("preset load "));
    for cmd in cmds {
        app.apply(cmd);
    }
    if presets {
        app.lists.lock().unwrap().stale = true;
    }
    if loaded {
        // A preset changes many settings at once; show what the service has now.
        app.c = crate::load_settings().0;
    }
    if restart {
        // Reopen once the camera is back, with the new size.
        app.preview.stop();
        app.view.hold = Some(Instant::now() + Duration::from_secs(6));
        app.view.camera_preview = false;
    }
}

/// Lays out `add` in a column `width` wide, `left` from the edge.
fn column(ui: &mut Ui, left: f32, width: f32, add: impl FnOnce(&mut Ui)) {
    ui.horizontal_top(|ui| {
        ui.add_space(left.max(0.0));
        ui.vertical(|ui| {
            ui.set_width(width);
            add(ui);
        });
    });
}

/// Starts the preview, and restarts it when it fails or goes quiet.
fn keep_preview(app: &mut App, ctx: &egui::Context) {
    // Errors show in the preview itself, not as a message.
    if app.view.hold.is_some_and(|t| Instant::now() < t) || !app.want_preview {
        return;
    }
    if app.c.low_power && !app.view.camera_preview {
        if app.preview.running() {
            app.preview.stop();
        }
        return;
    }
    let since = app.preview.since_start();
    let retry = Duration::from_secs(if app.c.low_power { 10 } else { 3 });
    if app.preview.running() {
        // Only for the service output; the camera itself is left alone.
        if !app.c.low_power && !app.preview.live() && since.is_some_and(|d| d > Duration::from_secs(8)) {
            app.preview.stop();
        }
    } else if since.is_none_or(|d| d > retry) {
        app.start_preview(ctx);
    }
    ctx.request_repaint_after(Duration::from_millis(500));
}

fn header(ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Opal C1").size(24.0).strong().color(TEXT));
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Camera Control").color(WEAK));
    });
}

/// The live preview with status pills and the video off notice.
fn stage(app: &mut App, ui: &mut Ui, size: egui::Vec2) {
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), size.y), Sense::hover());
    let rect = Rect::from_center_size(row.center(), size);
    let radius = CornerRadius::same(18);
    let painter = ui.painter().clone();
    painter.rect_filled(rect, radius, Color32::from_rgb(0x08, 0x09, 0x0b));

    let connected = app.status.starts_with("mode ");
    let video_off = app.status_value("video") == Some("off");
    let running = app.preview.running();
    let showing = running && app.texture.is_some();
    if let (Some(t), true) = (&app.texture, showing) {
        egui::Image::new(t).corner_radius(radius).paint_at(ui, rect);
    }
    painter.rect_stroke(rect, radius, Stroke::new(1.0, LINE), egui::StrokeKind::Inside);
    if app.view.picking_white && showing {
        pick_white(app, ui, rect);
    }
    if let (Some(bins), true, true) = (app.bins, app.histogram, showing) {
        let r = Rect::from_min_size(rect.left_bottom() + vec2(14.0, -78.0), vec2(170.0, 64.0));
        painter.rect_filled(r, CornerRadius::same(8), Color32::from_black_alpha(150));
        let inner = r.shrink(6.0);
        let top = *bins.iter().max().unwrap_or(&1) as f32;
        let bar = inner.width() / bins.len() as f32;
        for (i, &n) in bins.iter().enumerate() {
            let h = inner.height() * (n as f32 / top.max(1.0)).sqrt();
            let x = inner.left() + i as f32 * bar;
            painter.rect_filled(
                Rect::from_min_max(pos2(x, inner.bottom() - h), pos2(x + bar, inner.bottom())),
                CornerRadius::ZERO,
                Color32::from_white_alpha(170),
            );
        }
    }

    let centre = |ui: &mut Ui, height: f32, add: &mut dyn FnMut(&mut Ui)| {
        let r = Rect::from_center_size(rect.center(), vec2(rect.width() - 40.0, height));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(r)
                .layout(Layout::top_down(Align::Center)),
        );
        add(&mut child);
    };
    if video_off && connected {
        ui.painter()
            .rect_filled(rect, radius, Color32::from_black_alpha(if showing { 200 } else { 0 }));
        centre(ui, 110.0, &mut |ui| {
            ui.label(egui::RichText::new("Your video is off").size(20.0).strong().color(TEXT));
            ui.label(egui::RichText::new("Make a peace sign to turn it back on, or").color(WEAK));
            ui.add_space(6.0);
            if theme::primary(ui, "Turn video back on").clicked() {
                app.apply("video on".into());
            }
        });
    } else if connected && app.c.low_power && !app.view.camera_preview {
        centre(ui, 110.0, &mut |ui| {
            ui.label(
                egui::RichText::new("Preview is off in low power mode")
                    .size(17.0)
                    .color(TEXT),
            );
            if cfg!(windows) {
                // The picture goes from the camera straight to apps, past the service.
                ui.label(egui::RichText::new("Apps get the picture straight from the camera.").color(WEAK));
                return;
            }
            ui.label(egui::RichText::new("Apps cannot use the camera while it shows.").color(WEAK));
            ui.add_space(6.0);
            if theme::primary(ui, "Show preview").clicked() {
                app.view.camera_preview = true;
            }
        });
    } else if !showing {
        let (title, detail) = if !connected {
            (
                "The camera service is not running",
                "Start it with: systemctl --user start opal-rs".to_owned(),
            )
        } else if !app.want_preview {
            ("Preview paused", String::new())
        } else if app.preview.error().is_some() {
            (
                "Waiting for the camera",
                "The preview will try again in a moment.".to_owned(),
            )
        } else {
            ("Starting preview", String::new())
        };
        let error = app.preview.error();
        centre(ui, 70.0, &mut |ui| {
            let r = ui.label(egui::RichText::new(title).size(17.0).color(TEXT));
            if let Some(e) = &error {
                r.on_hover_text(e);
            }
            if !detail.is_empty() {
                ui.label(egui::RichText::new(&detail).color(WEAK));
            }
        });
    }

    // In low power mode the preview holds the camera: offer to give it back to apps.
    if connected && app.c.low_power && app.view.camera_preview {
        let r = Rect::from_min_size(rect.right_bottom() + vec2(-140.0, -48.0), vec2(126.0, 34.0));
        if ui.put(r, egui::Button::new("Stop preview")).clicked() {
            app.view.camera_preview = false;
        }
    }

    // Status pills.
    let (text, dot) = if !connected {
        ("Not connected", RED)
    } else if video_off {
        ("Video off", AMBER)
    } else if matches!(app.status_value("loop"), Some("playing" | "bounce")) {
        ("Loop", AMBER)
    } else if app.status_value("streaming") == Some("true") {
        ("Live", GREEN)
    } else {
        ("Idle", WEAK)
    };
    let r = theme::pill(ui, rect.min + vec2(14.0, 14.0), Align2::LEFT_TOP, Some(dot), text);
    // Apps using the camera, other than this panel.
    let apps: Vec<&str> = app
        .status_value("apps")
        .unwrap_or("-")
        .split(',')
        .filter(|a| *a != "-" && *a != "opal-rs-gui")
        .collect();
    if connected && !apps.is_empty() {
        theme::pill(
            ui,
            r.right_top() + vec2(8.0, 0.0),
            Align2::LEFT_TOP,
            None,
            &apps.join(", "),
        );
    }
    if connected {
        let mut right = rect.right_top() + vec2(-14.0, 14.0);
        if let Some(t) = app.status_value("temperature").and_then(|t| t.parse::<f32>().ok()) {
            let r = theme::pill(ui, right, Align2::RIGHT_TOP, None, &format!("{t:.0} C"));
            right.x = r.left() - 8.0;
        }
        let res = if app.c.low_power {
            "Low power".to_owned()
        } else {
            app.status_value("resolution").unwrap_or(&app.c.resolution).to_owned()
        };
        theme::pill(ui, right, Align2::RIGHT_TOP, None, &res);
    }
}

fn message(app: &mut App, ui: &mut Ui) {
    // The preview already says when the service is down.
    if app.message.is_empty() || app.message.ends_with("service is not running") {
        return;
    }
    ui.add_space(10.0);
    egui::Frame::NONE
        .fill(Color32::from_rgb(0x3a, 0x1d, 0x20))
        .stroke(Stroke::new(1.0, Color32::from_rgb(0x5c, 0x2a, 0x2e)))
        .corner_radius(12)
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let width = ui.available_width() - 80.0;
                ui.allocate_ui(vec2(width, 0.0), |ui| {
                    let mut text = app.message.clone();
                    if let Some(f) = text.get_mut(..1) {
                        f.make_ascii_uppercase();
                    }
                    ui.add(egui::Label::new(egui::RichText::new(text).color(TEXT)).wrap());
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let b = egui::Button::new(egui::RichText::new("Dismiss").color(WEAK)).frame(false);
                    if ui.add(b).clicked() {
                        app.message.clear();
                    }
                });
            });
        });
}

/// Every setting in one list, grouped like the camera's own app: camera, audio, effects, manual, settings.
fn settings(app: &mut App, ui: &mut Ui, cmds: &mut Vec<String>) {
    let app_loop = app.status_value("loop").unwrap_or("off").to_owned();
    ui.spacing_mut().item_spacing.y = 10.0;
    if app.c.low_power {
        card(ui, "", |ui| {
            ui.label(
                egui::RichText::new("Effects are off in low power mode")
                    .size(15.5)
                    .color(AMBER),
            );
        });
    }
    let enabled = !app.c.low_power;
    let hand = app.status_value("hand") == Some("yes");
    let ctx = ui.ctx().clone();
    let (c, v) = (&mut app.c, &mut app.view);
    let mut message = None;

    theme::section(ui, "Camera");
    ui.add_enabled_ui(enabled, |ui| {
        card(ui, "", |ui| {
            ui.add_enabled_ui(!c.zoom, |ui| {
                let text = if c.magnify > 1.0 {
                    format!("{:.1}x", c.magnify)
                } else {
                    "Off".into()
                };
                if Slider::new("Zoom", &mut c.magnify, 1.0..=4.0, text)
                    .step(0.1)
                    .show(ui)
                    .changed()
                {
                    cmds.push(if c.magnify > 1.0 {
                        format!("magnify {:.1}", c.magnify)
                    } else {
                        "magnify off".into()
                    });
                }
            });
            if theme::switch_row(ui, "Follow my face", "", &mut c.zoom) {
                cmds.push(c.zoom_cmd());
            }
            if c.zoom {
                let text = format!("{:.0}%", c.zoom_amount * 100.0);
                if Slider::new("How close", &mut c.zoom_amount, 0.0..=1.0, text)
                    .show(ui)
                    .changed()
                {
                    cmds.push(c.zoom_cmd());
                }
                ui.label(egui::RichText::new("How quickly it follows").color(TEXT));
                let speeds = [
                    ("slow".to_owned(), "Slow"),
                    ("normal".to_owned(), "Normal"),
                    ("fast".to_owned(), "Fast"),
                ];
                if theme::segmented(ui, "tracking", &mut c.tracking, &speeds) {
                    cmds.push(format!("tracking {}", c.tracking));
                }
            }
        });
    });

    presets(v, ui, cmds, enabled);
    stickers(v, ui, cmds, enabled, &ctx);
    let looping = app_loop.clone();
    clip(v, ui, cmds, enabled, &looping);

    group(ui, "Effects");
    ui.add_enabled_ui(enabled, |ui| {
        card(ui, "", |ui| {
            if theme::switch_row(ui, "Blur background", "", &mut c.blur) {
                cmds.push(c.blur_cmd());
            }
            if c.blur {
                let text = c.blur_radius.to_string();
                if Slider::new("Strength", &mut c.blur_radius, 4..=64, text)
                    .show(ui)
                    .changed()
                {
                    cmds.push(c.blur_cmd());
                }
                if theme::switch_row(
                    ui,
                    "Lens look",
                    "Bright lights turn into soft shapes, as through a lens.",
                    &mut c.lens,
                ) {
                    cmds.push(format!("bokeh {}", if c.lens { "lens" } else { "soft" }));
                }
            }
        });

        card(ui, "", |ui| {
            let text = if c.touchup > 0 {
                format!("{}%", c.touchup)
            } else {
                "Off".into()
            };
            if Slider::new("Touch up", &mut c.touchup, 0..=100, text)
                .show(ui)
                .changed()
            {
                cmds.push(format!("touchup {}", c.touchup));
            }
            let even = format!("{}%", c.tone[0]);
            let a = Slider::new("Even skin tone", &mut c.tone[0], 0..=100, even).show(ui);
            let warm = signed(c.tone[1]);
            let b = Slider::new("Skin warmth", &mut c.tone[1], -100..=100, warm).show(ui);
            if a.changed() || b.changed() {
                cmds.push(format!("tone {} {}", c.tone[0], c.tone[1]));
            }
        });

        card(ui, "", |ui| {
            let text = if c.pixelate > 0 {
                format!("{}%", c.pixelate)
            } else {
                "Off".into()
            };
            if Slider::new("Pixelate", &mut c.pixelate, 0..=100, text)
                .show(ui)
                .changed()
            {
                cmds.push(if c.pixelate > 0 {
                    format!("pixelate {}", c.pixelate)
                } else {
                    "pixelate off".into()
                });
            }
        });

        card(ui, "", |ui| {
            let mut on = c.background_on || v.picking_background;
            if theme::switch_row(ui, "Background picture", "", &mut on) {
                v.picking_background = false;
                if !on {
                    if c.background_on {
                        c.background_on = false;
                        cmds.push("background off".into());
                    }
                } else if c.background.trim().is_empty() {
                    // Wait for a picture before turning it on.
                    v.picking_background = true;
                    if v.can_browse {
                        v.browse(&ctx, Pick::Background);
                    }
                } else {
                    c.background_on = true;
                    cmds.push(format!("background {}", c.background.trim()));
                }
            }
            if on {
                let (enter, browse) = file_row(ui, &mut c.background, "Picture file (JPEG or PNG)", v.can_browse);
                if enter {
                    if c.background.trim().is_empty() {
                        message = Some("Choose a picture first.".to_owned());
                    } else {
                        v.picking_background = false;
                        c.background_on = true;
                        cmds.push(format!("background {}", c.background.trim()));
                    }
                }
                if browse {
                    v.browse(&ctx, Pick::Background);
                }
            }
        });

        card(ui, "", |ui| {
            ui.label(egui::RichText::new("Look").size(15.5).color(TEXT));
            look_tiles(ui, c, v, cmds);
            if v.custom_look {
                let (enter, browse) = file_row(ui, &mut v.look_file, "Look file (.cube)", v.can_browse);
                if enter && !v.look_file.trim().is_empty() {
                    c.look = v.look_file.trim().to_owned();
                    c.look_on = true;
                    cmds.push(c.look_cmd());
                }
                if browse {
                    v.browse(&ctx, Pick::Look);
                }
            }
            if c.look_on {
                let text = format!("{:.0}%", c.look_amount * 100.0);
                let s = Slider::new("Strength", &mut c.look_amount, 0.0..=1.0, text).show(ui);
                if s.drag_stopped() || (s.changed() && !s.dragged()) {
                    cmds.push(c.look_cmd());
                }
            }
        });
    });

    group(ui, "Manual");
    let arrow = if v.more { Arrow::Up } else { Arrow::Down };
    if link_card(ui, "Focus, exposure and colour", arrow) {
        v.more = !v.more;
    }
    if v.more {
        manual(app, ui, cmds);
    }

    #[cfg(unix)]
    {
        group(ui, "Virtual cameras");
        let reply = app.lists.lock().unwrap().cameras.clone();
        if app.view.cameras.show(ui, &reply) {
            app.lists.lock().unwrap().stale = true;
        }
    }

    let c = &mut app.c;
    group(ui, "Microphone");
    card(ui, "", |ui| {
        if theme::switch_row(ui, "Microphone", "", &mut c.mic) {
            cmds.push(format!("mic {}", if c.mic { "on" } else { "off" }));
        }
        ui.add_enabled_ui(c.mic, |ui| {
            let text = if c.gain > 0.0 {
                format!("+{:.1} dB", c.gain)
            } else {
                "Off".into()
            };
            if Slider::new("Boost", &mut c.gain, 0.0..=30.0, text)
                .step(0.5)
                .show(ui)
                .changed()
            {
                cmds.push(format!("gain {:.1}", c.gain));
            }
        });
    });

    group(ui, "Settings");
    ui.add_enabled_ui(enabled, |ui| {
        card(ui, "", |ui| {
            if theme::switch_row(ui, "Hand gestures", "", &mut c.gestures) {
                cmds.push(c.gestures_cmd());
            }
            if c.gestures {
                for (name, label, on) in [
                    ("peace", "Peace sign turns the video off and on", &mut c.peace),
                    ("pinch", "Pinch turns Follow my face on and off", &mut c.pinch),
                ] {
                    if theme::switch_row(ui, label, "", on) {
                        cmds.push(format!("gesture {name} {}", if *on { "on" } else { "off" }));
                    }
                }
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter()
                        .circle_filled(r.center(), 4.0, if hand { GREEN } else { RAISED });
                    let text = if hand {
                        "Your hand is in view"
                    } else {
                        "No hand in view"
                    };
                    ui.label(egui::RichText::new(text).small().color(WEAK));
                });
            }
        });
    });
    card(ui, "", |ui| {
        let (mut zebra, mut histogram) = (app.zebra, app.histogram);
        let a = theme::switch_row(
            ui,
            "Zebra stripes",
            "Marks parts of the preview that are too bright.",
            &mut zebra,
        );
        let b = theme::switch_row(
            ui,
            "Histogram",
            "Shows how bright the picture is, dark on the left.",
            &mut histogram,
        );
        if a || b {
            (app.zebra, app.histogram) = (zebra, histogram);
            crate::overlay::save(zebra, histogram);
        }
    });
    device(app, ui, cmds);
    if link_card(ui, "Help", Arrow::Right) {
        app.view.help = true;
        app.view.help_top = true;
    }
    if link_card(ui, "Debug mode", Arrow::Right) {
        app.debug = true;
    }
    if let Some(m) = message {
        app.message = m;
    }
}

/// A section heading with room above it.
fn group(ui: &mut Ui, title: &str) {
    ui.add_space(8.0);
    theme::section(ui, title);
}

#[derive(Clone, Copy)]
enum Arrow {
    Down,
    Up,
    Right,
}

/// A clickable card with a title and an arrow; returns true when clicked.
fn link_card(ui: &mut Ui, title: &str, arrow: Arrow) -> bool {
    let r = egui::Frame::NONE
        .fill(CARD)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(14)
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(title).size(15.5).color(TEXT));
        })
        .response;
    let resp = ui.interact(r.rect, ui.id().with(("link", title)), Sense::click());
    let c = pos2(r.rect.right() - 26.0, r.rect.center().y);
    let stroke = Stroke::new(2.0, if resp.hovered() { TEXT } else { WEAK });
    let (a, b, d) = match arrow {
        Arrow::Down => (vec2(-6.0, -3.0), vec2(6.0, -3.0), vec2(0.0, 3.0)),
        Arrow::Up => (vec2(-6.0, 3.0), vec2(6.0, 3.0), vec2(0.0, -3.0)),
        Arrow::Right => (vec2(-3.0, -6.0), vec2(-3.0, 6.0), vec2(3.0, 0.0)),
    };
    ui.painter().line_segment([c + a, c + d], stroke);
    ui.painter().line_segment([c + d, c + b], stroke);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// A file path box with a Browse button; returns (Enter pressed, Browse clicked).
fn file_row(ui: &mut Ui, path: &mut String, placeholder: &str, browse: bool) -> (bool, bool) {
    let mut browse_clicked = false;
    let enter = ui
        .horizontal(|ui| {
            let button_w = if browse { 86.0 } else { 0.0 };
            let edit = egui::TextEdit::singleline(path)
                .hint_text(placeholder)
                .desired_width(ui.available_width() - button_w)
                .margin(vec2(10.0, 7.0));
            let r = ui.add(edit);
            if browse {
                browse_clicked = ui.button("Browse").clicked();
            }
            r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
        })
        .inner;
    (enter, browse_clicked)
}

/// Paints a tiny portrait in `rect` with the given colours.
fn paint_swatch(ui: &Ui, rect: Rect, s: &Swatch) {
    let p = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let r = 8u8;
    p.rect_filled(rect, CornerRadius::same(r), s[0]);
    let wall = Rect::from_min_max(pos2(rect.left(), rect.top() + rect.height() * 0.62), rect.max);
    p.rect_filled(
        wall,
        CornerRadius {
            nw: 0,
            ne: 0,
            sw: r,
            se: r,
        },
        s[1],
    );
    let h = rect.height();
    let body = Rect::from_center_size(
        pos2(rect.center().x, rect.bottom() - h * 0.14),
        vec2(rect.width() * 0.46, h * 0.36),
    );
    p.rect_filled(
        body.intersect(rect),
        CornerRadius {
            nw: 12,
            ne: 12,
            sw: 0,
            se: 0,
        },
        s[3],
    );
    p.circle_filled(pos2(rect.center().x, rect.top() + h * 0.42), h * 0.2, s[2]);
}

fn look_tiles(ui: &mut Ui, c: &mut Controls, v: &mut State, cmds: &mut Vec<String>) {
    // One row that scrolls sideways, also with the mouse wheel.
    let out = ui
        .scope(|ui| {
            ui.style_mut().always_scroll_the_only_direction = true;
            let mut area = egui::ScrollArea::horizontal()
                .id_salt("looks")
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden);
            if v.reveal_look {
                // Start with the chosen look in the middle.
                let index = if v.custom_look {
                    opal_effects::LOOKS.len() + 1
                } else if c.look_on {
                    opal_effects::LOOKS
                        .iter()
                        .position(|n| *n == c.look)
                        .map_or(0, |i| i + 1)
                } else {
                    0
                };
                let x = index as f32 * (TILE_W + TILE_GAP) - (ui.available_width() - TILE_W) / 2.0;
                area = area.horizontal_scroll_offset(x.max(0.0));
            }
            area.show(ui, |ui| look_row(ui, c, v, cmds))
        })
        .inner;
    v.reveal_look = false;
    // Fade the edges where more looks are hidden.
    let x = out.state.offset.x;
    let r = Rect::from_min_size(out.inner_rect.min, vec2(out.inner_rect.width(), out.content_size.y));
    let more = out.content_size.x - r.width() - x;
    let fade = |left: bool| {
        let w = 28.0;
        let (x0, x1) = if left {
            (r.left(), r.left() + w)
        } else {
            (r.right(), r.right() - w)
        };
        let mut mesh = egui::Mesh::default();
        let clear = Color32::TRANSPARENT;
        mesh.colored_vertex(pos2(x0, r.top()), CARD);
        mesh.colored_vertex(pos2(x1, r.top()), clear);
        mesh.colored_vertex(pos2(x1, r.bottom()), clear);
        mesh.colored_vertex(pos2(x0, r.bottom()), CARD);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        ui.painter().add(mesh);
    };
    if x > 1.0 {
        fade(true);
    }
    if more > 1.0 {
        fade(false);
    }
}

fn look_row(ui: &mut Ui, c: &mut Controls, v: &mut State, cmds: &mut Vec<String>) {
    let names = opal_effects::LOOKS;
    let count = names.len() + 2;
    let (w, gap) = (TILE_W, TILE_GAP);
    let swatch_h = 54.0;
    let h = swatch_h + 24.0;
    let row_w = count as f32 * (w + gap) - gap;
    let (area, _) = ui.allocate_exact_size(vec2(row_w, h), Sense::hover());
    for i in 0..count {
        let rect = Rect::from_min_size(area.min + vec2(i as f32 * (w + gap), 0.0), vec2(w, h));
        let resp = ui.interact(rect, ui.id().with(("look", i)), Sense::click());
        let (label, selected) = if i == 0 {
            ("None".to_owned(), !v.custom_look && !c.look_on)
        } else if i == count - 1 {
            ("Your own".to_owned(), v.custom_look)
        } else {
            let name = names[i - 1];
            (title_case(name), !v.custom_look && c.look_on && c.look == name)
        };
        let p = ui.painter();
        let sw = Rect::from_min_size(rect.min, vec2(w, swatch_h)).shrink(3.0);
        if i == count - 1 {
            p.rect_filled(sw, CornerRadius::same(8), RAISED);
            let (cx, s) = (sw.center(), 7.0);
            let st = Stroke::new(2.0, WEAK);
            p.line_segment([cx - vec2(s, 0.0), cx + vec2(s, 0.0)], st);
            p.line_segment([cx - vec2(0.0, s), cx + vec2(0.0, s)], st);
        } else {
            let s = if i == 0 { &v.plain } else { &v.swatches[i - 1] };
            paint_swatch(ui, sw, s);
        }
        let p = ui.painter();
        if selected {
            p.rect_stroke(
                rect,
                CornerRadius::same(11),
                Stroke::new(2.0, ACCENT),
                egui::StrokeKind::Inside,
            );
        } else if resp.hovered() {
            p.rect_stroke(
                rect,
                CornerRadius::same(11),
                Stroke::new(1.0, Color32::from_rgb(0x44, 0x48, 0x52)),
                egui::StrokeKind::Inside,
            );
        }
        p.text(
            pos2(rect.center().x, rect.bottom() - 12.0),
            Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(12.5),
            if selected { TEXT } else { WEAK },
        );
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            if i == 0 {
                v.custom_look = false;
                if c.look_on {
                    c.look_on = false;
                    cmds.push("lut off".into());
                }
            } else if i == count - 1 {
                v.custom_look = true;
                if !v.look_file.trim().is_empty() {
                    c.look = v.look_file.trim().to_owned();
                    c.look_on = true;
                    cmds.push(c.look_cmd());
                }
            } else {
                v.custom_look = false;
                c.look = names[i - 1].to_owned();
                c.look_on = true;
                cmds.push(c.look_cmd());
            }
        }
    }
}

/// Resolution, when the camera runs, and low power mode.
fn device(app: &mut App, ui: &mut Ui, cmds: &mut Vec<String>) {
    let info = |k| app.status_value(k).map(str::to_owned);
    let (usb, serial, firmware) = (info("usb"), info("serial"), info("firmware"));
    let (c, mode) = (&mut app.c, &mut app.mode);
    card(ui, "", |ui| {
        if matches!(usb.as_deref(), Some("low" | "full" | "high")) {
            ui.label(
                egui::RichText::new("The camera is on a slow USB port, so video is limited to 720p. Use a USB 3 port.")
                    .color(AMBER),
            );
            ui.add_space(4.0);
        }
        ui.add_enabled_ui(!c.low_power, |ui| {
            ui.label(egui::RichText::new("Resolution").color(TEXT));
            let options = RESOLUTIONS.map(|(n, _, _)| (n.to_owned(), n));
            if theme::segmented(ui, "resolution", &mut c.resolution, &options) {
                cmds.push(format!("resolution {}", c.resolution));
            }
            hint(ui, "Changing this restarts the camera.");
        });
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Light flicker").color(TEXT));
        let options = [
            ("auto".to_owned(), "Auto"),
            ("50".to_owned(), "50 Hz"),
            ("60".to_owned(), "60 Hz"),
            ("off".to_owned(), "Off"),
        ];
        if theme::segmented(ui, "antibanding", &mut c.antibanding, &options) {
            cmds.push(format!("antibanding {}", c.antibanding));
        }
        hint(ui, "Removes flicker from mains lighting. Auto goes by your time zone.");
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Camera on").color(TEXT));
        let modes = [(Mode::Auto, "When needed"), (Mode::On, "Always"), (Mode::Off, "Never")];
        if theme::segmented(ui, "mode", mode, &modes) {
            cmds.push(
                match mode {
                    Mode::Auto => "auto",
                    Mode::On => "on",
                    Mode::Off => "off",
                }
                .into(),
            );
        }
        if theme::switch_row(ui, "Off while the screen is locked", "", &mut c.lock_pause) {
            cmds.push(format!("lockpause {}", if c.lock_pause { "on" } else { "off" }));
        }
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        if theme::switch_row(
            ui,
            "Low power mode",
            "Changing this restarts the camera.",
            &mut c.low_power,
        ) {
            cmds.push(format!("power {}", if c.low_power { "low" } else { "normal" }));
        }
        if let (Some(serial), Some(firmware), Some(usb)) = (&serial, &firmware, &usb) {
            ui.add_space(4.0);
            hint(ui, &format!("Serial {serial}, firmware {firmware}, USB {usb} speed"));
        }
    });
}

/// Focus, exposure, colour and picture adjustments.
fn manual(app: &mut App, ui: &mut Ui, cmds: &mut Vec<String>) {
    let c = &mut app.c;
    card(ui, "Focus", |ui| {
        if theme::switch_row(ui, "Autofocus", "", &mut c.auto_focus) {
            cmds.push(if c.auto_focus {
                "af".into()
            } else {
                format!("focus {}", c.focus)
            });
        }
        if !c.auto_focus {
            let text = c.focus.to_string();
            if Slider::new("Focus", &mut c.focus, 0..=255, text).show(ui).changed() {
                cmds.push(format!("focus {}", c.focus));
            }
        }
    });

    card(ui, "Exposure", |ui| {
        if theme::switch_row(ui, "Automatic exposure", "", &mut c.auto_exposure) {
            cmds.push(if c.auto_exposure {
                "ae".into()
            } else {
                format!("exposure {} {}", c.exposure_us, c.iso)
            });
        }
        if c.auto_exposure {
            let text = signed(c.ev);
            if Slider::new("Brighter or darker", &mut c.ev, -9..=9, text)
                .show(ui)
                .changed()
            {
                cmds.push(format!("ev {}", c.ev));
            }
        } else {
            let text = format!("1/{} s", (1_000_000.0 / c.exposure_us as f32).round());
            let a = Slider::new("Shutter speed", &mut c.exposure_us, 100..=33000, text)
                .log()
                .step(1.0)
                .show(ui);
            let text = c.iso.to_string();
            let b = Slider::new("Sensitivity (ISO)", &mut c.iso, 100..=1600, text).show(ui);
            if a.changed() || b.changed() {
                cmds.push(format!("exposure {} {}", c.exposure_us, c.iso));
            }
        }
    });

    card(ui, "Colour", |ui| {
        if theme::switch_row(ui, "Automatic white balance", "", &mut c.auto_wb) {
            cmds.push(if c.auto_wb {
                "awb".into()
            } else {
                format!("wb {}", c.wb)
            });
        }
        if !c.auto_wb {
            let text = format!("{} K", c.wb);
            if Slider::new("Colour temperature", &mut c.wb, 1000..=12000, text)
                .step(50.0)
                .show(ui)
                .changed()
            {
                cmds.push(format!("wb {}", c.wb));
            }
        }
    });

    card(ui, "", |ui| {
        ui.horizontal(|ui| {
            theme::section(ui, "Picture");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let default = (c.brightness, c.contrast, c.saturation, c.sharpness) == (0, 0, 0, 1);
                if ui.add_enabled(!default, egui::Button::new("Reset").small()).clicked() {
                    (c.brightness, c.contrast, c.saturation, c.sharpness) = (0, 0, 0, 1);
                    cmds.extend(["brightness 0", "contrast 0", "saturation 0", "sharpness 1"].map(String::from));
                }
            });
        });
        ui.add_space(2.0);
        for (name, label, value, range) in [
            ("brightness", "Brightness", &mut c.brightness, -10..=10),
            ("contrast", "Contrast", &mut c.contrast, -10..=10),
            ("saturation", "Colour strength", &mut c.saturation, -10..=10),
            ("sharpness", "Sharpness", &mut c.sharpness, 0..=4),
        ] {
            let text = if name == "sharpness" {
                value.to_string()
            } else {
                signed(*value)
            };
            if Slider::new(label, value, range, text).show(ui).changed() {
                cmds.push(format!("{name} {value}"));
            }
        }
    });
    lighting(app, ui, cmds);
}

/// While picking: a crosshair over the preview, and a click sets temperature and tint so that spot turns grey.
fn pick_white(app: &mut App, ui: &mut Ui, rect: Rect) {
    let resp = ui.interact(rect, ui.id().with("pick white"), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::Crosshair);
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.view.picking_white = false;
        return;
    }
    let (Some(pos), true) = (resp.interact_pointer_pos(), resp.clicked()) else {
        return;
    };
    let Some((rgb, [w, h])) = &app.frame else {
        return;
    };
    let u = (pos - rect.min) / rect.size();
    let (cx, cy) = ((u.x * *w as f32) as usize, (u.y * *h as f32) as usize);
    let mut sum = [0f32; 3];
    let mut n = 0.0;
    for y in cy.saturating_sub(2)..(cy + 3).min(*h) {
        for x in cx.saturating_sub(2)..(cx + 3).min(*w) {
            for (c, s) in sum.iter_mut().enumerate() {
                *s += rgb[(y * w + x) * 3 + c] as f32;
            }
            n += 1.0;
        }
    }
    if n == 0.0 {
        return;
    }
    let l = &mut app.c.lighting;
    let (t, k) = opal_effects::neutralise(sum.map(|s| s / n), l[0] as f32 / 100.0, l[1] as f32 / 100.0);
    (l[0], l[1]) = ((t * 100.0).round() as i32, (k * 100.0).round() as i32);
    let cmds = [format!("warmth {}", l[0]), format!("tint {}", l[1])];
    app.view.picking_white = false;
    for c in cmds {
        app.apply(c);
    }
}

/// Loop: hold the button to record a few seconds, which then play instead of the live video.
fn clip(v: &mut State, ui: &mut Ui, cmds: &mut Vec<String>, enabled: bool, state: &str) {
    group(ui, "Loop");
    ui.add_enabled_ui(enabled, |ui| {
        card(ui, "", |ui| {
            let playing = state.starts_with("playing") || state.starts_with("bounce");
            let label = if v.loop_holding {
                "Recording..."
            } else {
                "Hold to record"
            };
            let resp = ui.add(egui::Button::new(label).min_size(vec2(ui.available_width(), 34.0)));
            let down = resp.is_pointer_button_down_on();
            if down && !v.loop_holding {
                v.loop_holding = true;
                cmds.push("loop record".into());
            } else if !down && v.loop_holding {
                v.loop_holding = false;
                cmds.push("loop stop".into());
                cmds.push(format!("loop {}", if v.loop_bounce { "bounce" } else { "play" }));
            }
            if theme::switch_row(
                ui,
                "Bounce",
                "Plays forwards then backwards instead of starting over.",
                &mut v.loop_bounce,
            ) && playing
            {
                cmds.push(format!("loop {}", if v.loop_bounce { "bounce" } else { "play" }));
            }
            if playing && ui.button("Back to live video").clicked() {
                cmds.push("loop off".into());
            }
        });
    });
}

/// Stickers over the video: a list to edit and remove them, and a form to add one.
fn stickers(v: &mut State, ui: &mut Ui, cmds: &mut Vec<String>, enabled: bool, ctx: &egui::Context) {
    group(ui, "Stickers");
    ui.add_enabled_ui(enabled, |ui| {
        card(ui, "", |ui| {
            let mut remove = None;
            for row in &mut v.stickers {
                let open = v.sticker.as_deref() == Some(row.id.as_str());
                ui.horizontal(|ui| {
                    if ui.selectable_label(open, row.label()).clicked() {
                        v.sticker = if open { None } else { Some(row.id.clone()) };
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Remove").clicked() {
                            remove = Some(row.id.clone());
                        }
                    });
                });
                if open {
                    let mut moved = false;
                    let pct = |v: f32| format!("{:.0}%", v * 100.0);
                    let (x, y, size) = (pct(row.x), pct(row.y), pct(row.size));
                    moved |= Slider::new("Across", &mut row.x, 0.0..=1.0, x).show(ui).changed();
                    moved |= Slider::new("Down", &mut row.y, 0.0..=1.0, y).show(ui).changed();
                    moved |= Slider::new("Size", &mut row.size, 0.02..=0.5, size).show(ui).changed();
                    if moved {
                        cmds.push(format!(
                            "sticker move {} {:.3} {:.3} {:.3}",
                            row.id, row.x, row.y, row.size
                        ));
                    }
                    let (mut behind, mut follow) = (row.flags.contains('b'), row.flags.contains('f'));
                    let a = theme::switch_row(ui, "Behind me", "", &mut behind);
                    let b = theme::switch_row(
                        ui,
                        "Follow my face",
                        "Across and down are then around your face.",
                        &mut follow,
                    );
                    if a || b {
                        row.flags = match (behind, follow) {
                            (false, false) => "-",
                            (true, false) => "b",
                            (false, true) => "f",
                            (true, true) => "bf",
                        }
                        .into();
                        cmds.push(format!("sticker flags {} {}", row.id, row.flags));
                    }
                    ui.separator();
                }
            }
            if let Some(id) = remove {
                cmds.push(format!("sticker remove {id}"));
                v.stickers.retain(|r| r.id != id);
            }
            if !v.stickers.is_empty() {
                ui.add_space(6.0);
            }
            let kinds = [
                ("text".to_owned(), "Text"),
                ("card".to_owned(), "Name card"),
                ("clock".to_owned(), "Clock"),
                ("timer".to_owned(), "Timer"),
                ("picture".to_owned(), "Picture"),
            ];
            theme::segmented(ui, "sticker kind", &mut v.new_kind, &kinds);
            let line = |ui: &mut Ui, text: &mut String, hint: &str| {
                ui.add(
                    egui::TextEdit::singleline(text)
                        .hint_text(hint)
                        .desired_width(ui.available_width())
                        .margin(vec2(10.0, 7.0)),
                );
            };
            match v.new_kind.as_str() {
                "text" => line(ui, &mut v.new_text, "Words to show"),
                "card" => {
                    line(ui, &mut v.new_text, "Name");
                    line(ui, &mut v.new_title, "Title or role");
                }
                "picture" => {
                    let (_, browse) = file_row(ui, &mut v.new_picture, "Picture file (PNG or JPEG)", v.can_browse);
                    if browse {
                        v.browse(ctx, Pick::Sticker);
                    }
                }
                _ => {}
            }
            if matches!(v.new_kind.as_str(), "text" | "clock" | "timer") {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Colour").color(TEXT));
                    egui::ComboBox::from_id_salt("sticker colour")
                        .selected_text(title_case(&v.new_colour))
                        .show_ui(ui, |ui| {
                            for c in [
                                "white", "black", "yellow", "orange", "red", "pink", "purple", "blue", "green", "grey",
                            ] {
                                ui.selectable_value(&mut v.new_colour, c.to_owned(), title_case(c));
                            }
                        });
                });
            }
            let words = v.new_text.split_whitespace().collect::<Vec<_>>().join(" ");
            let title = v.new_title.split_whitespace().collect::<Vec<_>>().join(" ");
            let command = match v.new_kind.as_str() {
                "text" if !words.is_empty() => Some(format!("text 0.5 0.5 0.08 - {} {words}", v.new_colour)),
                "card" if !words.is_empty() => Some(format!("card 0.25 0.84 0.14 - {words} | {title}")),
                "clock" => Some(format!("clock 0.9 0.07 0.06 - {}", v.new_colour)),
                "timer" => Some(format!("timer 0.9 0.07 0.06 - {}", v.new_colour)),
                "picture" if !v.new_picture.trim().is_empty() => {
                    Some(format!("picture 0.15 0.2 0.2 - {}", v.new_picture.trim()))
                }
                _ => None,
            };
            if ui
                .add_enabled(command.is_some(), egui::Button::new("Add sticker"))
                .clicked()
                && let Some(c) = command
            {
                cmds.push(format!("sticker add {c}"));
                v.new_text.clear();
                v.new_title.clear();
            }
        });
    });
}

/// Saved looks: click one to use it, right-click to delete it, or save the current settings under a name.
fn presets(v: &mut State, ui: &mut Ui, cmds: &mut Vec<String>, enabled: bool) {
    group(ui, "Presets");
    ui.add_enabled_ui(enabled, |ui| {
        card(ui, "", |ui| {
            if v.presets.is_empty() {
                hint(
                    ui,
                    "Save your look, effects and lighting here to switch back to them later.",
                );
            } else {
                ui.horizontal_wrapped(|ui| {
                    for name in &v.presets {
                        let resp = ui.button(name.as_str());
                        if resp.clicked() {
                            cmds.push(format!("preset load {name}"));
                        }
                        resp.context_menu(|ui| {
                            if ui.button("Delete").clicked() {
                                cmds.push(format!("preset delete {name}"));
                                ui.close();
                            }
                        });
                    }
                });
            }
            ui.horizontal(|ui| {
                let edit = egui::TextEdit::singleline(&mut v.preset_name)
                    .hint_text("Name")
                    .desired_width(ui.available_width() - 70.0)
                    .margin(vec2(10.0, 7.0));
                let r = ui.add(edit);
                let name: String = v
                    .preset_name
                    .trim()
                    .chars()
                    .map(|c| if c == ' ' { '_' } else { c })
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                    .collect();
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.add_enabled(!name.is_empty(), egui::Button::new("Save")).clicked() || enter) && !name.is_empty()
                {
                    cmds.push(format!("preset save {name}"));
                    v.preset_name.clear();
                }
            });
        });
    });
}

/// Lighting and per-colour adjustments, done on this computer.
fn lighting(app: &mut App, ui: &mut Ui, cmds: &mut Vec<String>) {
    let (c, v) = (&mut app.c, &mut app.view);
    ui.add_enabled_ui(!c.low_power, |ui| lighting_cards(c, v, ui, cmds));
}

fn lighting_cards(c: &mut Controls, v: &mut State, ui: &mut Ui, cmds: &mut Vec<String>) {
    card(ui, "", |ui| {
        ui.horizontal(|ui| {
            theme::section(ui, "Lighting");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let neutral = c.lighting == [0; 5] && c.colours == [[0; 3]; 8];
                if ui.add_enabled(!neutral, egui::Button::new("Reset").small()).clicked() {
                    (c.lighting, c.colours) = ([0; 5], [[0; 3]; 8]);
                    cmds.push("adjust reset".into());
                }
            });
        });
        ui.add_space(2.0);
        let label = if v.picking_white {
            "Click something white or grey in the preview"
        } else {
            "Pick white"
        };
        if ui.button(label).clicked() {
            v.picking_white = !v.picking_white;
        }
        let labels = ["Temperature", "Tint", "Shadows", "Highlights", "Vibrance"];
        for ((name, label), value) in LIGHTING.iter().zip(labels).zip(c.lighting.iter_mut()) {
            let text = signed(*value);
            if Slider::new(label, value, -100..=100, text).show(ui).changed() {
                cmds.push(format!("{name} {value}"));
            }
        }
    });
    card(ui, "Colours", |ui| {
        ui.horizontal(|ui| {
            for (i, (_, hue)) in opal_effects::COLOURS.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
                let colour = egui::ecolor::Hsva::new(hue / 360.0, 0.75, 0.9, 1.0);
                ui.painter().circle_filled(rect.center(), 11.0, Color32::from(colour));
                if v.colour == i {
                    ui.painter().circle_stroke(rect.center(), 13.5, Stroke::new(2.0, TEXT));
                }
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    v.colour = i;
                }
            }
        });
        let (name, _) = opal_effects::COLOURS[v.colour];
        let values = &mut c.colours[v.colour];
        let mut changed = false;
        for (label, value) in ["Hue", "Saturation", "Lightness"].into_iter().zip(values.iter_mut()) {
            let text = signed(*value);
            changed |= Slider::new(label, value, -100..=100, text).show(ui).changed();
        }
        if changed {
            cmds.push(format!("colour {name} {} {} {}", values[0], values[1], values[2]));
        }
    });
}

/// What each setting does, in a few words.
const HELP: [(&str, &str); 22] = [
    (
        "Zoom",
        "Zooms into the middle of the picture. Follow my face takes over while it is on.",
    ),
    (
        "Follow my face",
        "Keeps you in the middle of the picture as you move around. How close sets how tightly you are framed, \
         and how quickly it follows sets how fast the picture catches up with you.",
    ),
    (
        "Presets",
        "Saves your look, lighting, colours, blur or background picture and framing under a name. Click a preset to \
         switch to it, or right-click it to delete it.",
    ),
    (
        "Blur background",
        "Softens everything behind you so only you stay sharp. Strength sets how soft it gets.",
    ),
    (
        "Loop",
        "Hold the button to record up to ten seconds, then let go: the recording plays in a loop instead of your \
         live video until you go back to live. Bounce plays it forwards then backwards.",
    ),
    (
        "Stickers",
        "Words, a name card, a clock, a timer or a picture on top of your video. Click one to move it, size it, put \
         it behind you or have it follow your face.",
    ),
    (
        "Touch up and skin tone",
        "Touch up softens fine detail on skin and leaves eyes and edges sharp. Even skin tone smooths out blotchy \
         colour, and skin warmth makes skin a little warmer or cooler. Skin is found by its colour.",
    ),
    (
        "Pixelate",
        "Turns the whole picture into big squares. Higher makes the squares bigger.",
    ),
    (
        "Background picture",
        "Shows a picture of your choice behind you instead of the room. Use a JPEG or PNG file.",
    ),
    (
        "Look",
        "Gives your video a different colour style. Strength blends between your normal picture and the look. \
         Your own uses a look file (.cube) from a photo or video editing app.",
    ),
    (
        "Focus, exposure and colour",
        "The automatic settings suit most rooms. Turn one off to set it by hand: focus for distance, shutter speed \
         and sensitivity for brightness, and colour temperature if the picture looks too blue or too orange.",
    ),
    (
        "Picture",
        "Fine tune brightness, contrast, colour strength and sharpness. Reset puts them back.",
    ),
    (
        "Lighting",
        "Temperature makes the picture warmer or cooler, tint more magenta or green. Shadows and highlights brighten \
         or darken the dark and light parts, and vibrance makes dull colours stronger while leaving skin alone. \
         Pick white, then click something white or grey in the preview, sets temperature and tint for you.",
    ),
    (
        "Colours",
        "Pick a colour, then shift its hue or make it stronger, weaker, lighter or darker. Other colours stay as \
         they are.",
    ),
    (
        "Microphone",
        "Turns the camera's microphone off or on. Boost makes your voice louder if people find you hard to hear.",
    ),
    (
        "Hand gestures",
        "Hold up a peace sign to turn your video off, and again to turn it back on. Pinch to turn Follow my face \
         on or off.",
    ),
    (
        "Resolution",
        "How sharp the picture is. Higher is sharper but asks more of your computer. Changing it restarts the camera.",
    ),
    (
        "Light flicker",
        "Removes the flicker or rolling bands that mains-powered lights can cause. Auto picks 50 or 60 Hz from your \
         time zone; pick one yourself if the bands stay.",
    ),
    (
        "Camera on",
        "When needed starts the camera only while an app is using it. Always keeps it running so apps start faster. \
         Never keeps it off. Off while the screen is locked stops it whenever you lock your screen.",
    ),
    (
        "Low power mode",
        "Apps get the picture straight from the camera at 1080p. This uses less power, but the effects, Follow my \
         face and hand gestures are off. Changing it restarts the camera.",
    ),
    (
        "Zebra stripes and histogram",
        "Help with exposure. Stripes mark parts of the preview that are too bright to show detail, and the \
         histogram shows how much of the picture is dark (left) or bright (right). Only the preview shows them.",
    ),
    (
        "Debug mode",
        "Every setting in a plain list, with the raw camera status. Useful when reporting a problem.",
    ),
];

fn help(app: &mut App, ui: &mut Ui) {
    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
        let mut area = egui::ScrollArea::vertical().id_salt("help").auto_shrink(false);
        if std::mem::take(&mut app.view.help_top) {
            area = area.vertical_scroll_offset(0.0);
        }
        area.show(ui, |ui| {
            egui::Frame::NONE.inner_margin(Margin::same(16)).show(ui, |ui| {
                let w = ui.available_width().min(COLUMN);
                ui.horizontal_top(|ui| {
                    ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                    ui.vertical(|ui| {
                        ui.set_width(w);
                        ui.horizontal(|ui| {
                            if ui.button("Back").clicked() {
                                app.view.help = false;
                            }
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new("Help").size(24.0).strong().color(TEXT));
                        });
                        ui.add_space(10.0);
                        ui.spacing_mut().item_spacing.y = 10.0;
                        for (title, text) in HELP {
                            card(ui, "", |ui| {
                                ui.spacing_mut().item_spacing.y = 4.0;
                                ui.label(egui::RichText::new(title).size(15.5).color(TEXT));
                                ui.add(egui::Label::new(egui::RichText::new(text).color(WEAK)).wrap());
                            });
                        }
                    });
                });
            });
        });
    });
}
