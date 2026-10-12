//! First run: opens Opal's download page and extracts the camera files from the disk image the user picks.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText, Ui};

use crate::theme::{RED, WEAK};
use crate::view::{can_browse, file_dialog};

/// Opal's own page for Opal Composer downloads.
const OPAL_PAGE: &str = "https://op.al/composer";
const DMG_NAME: &str = "Opal_Composer_2.0.0_24.dmg";

#[derive(Clone, PartialEq)]
enum Phase {
    Checking,
    Missing,
    Extracting,
    Ready,
    Failed(String),
}

pub struct Setup {
    phase: Arc<Mutex<Phase>>,
    /// Files written so far while extracting.
    progress: Arc<Mutex<Vec<String>>>,
    /// The path typed in, or picked.
    path: Arc<Mutex<String>>,
    can_browse: bool,
}

/// A command for one of the opal-rs programs, without a console window on Windows.
fn tool(args: &[&str]) -> Command {
    let mut c = Command::new(opal_ipc::program("opal-rs"));
    c.args(args);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut c, opal_ipc::NO_WINDOW);
    c
}

impl Setup {
    /// Starts by checking the camera files in the background.
    pub fn new(ctx: &egui::Context) -> Self {
        let setup = Self {
            phase: Arc::new(Mutex::new(Phase::Checking)),
            progress: Arc::default(),
            path: Arc::default(),
            can_browse: can_browse(),
        };
        let (phase, ctx) = (setup.phase.clone(), ctx.clone());
        std::thread::spawn(move || {
            let ok = tool(&["assets", "verify"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            *phase.lock().unwrap() = if ok { Phase::Ready } else { Phase::Missing };
            ctx.request_repaint();
        });
        setup
    }

    /// True until the camera files are in place.
    pub fn blocking(&self) -> bool {
        *self.phase.lock().unwrap() != Phase::Ready
    }

    pub fn show(&mut self, ui: &mut Ui) {
        egui::Frame::new().inner_margin(16).show(ui, |ui| self.contents(ui));
    }

    fn contents(&mut self, ui: &mut Ui) {
        let phase = self.phase.lock().unwrap().clone();
        ui.add_space(24.0);
        ui.vertical_centered(|ui| ui.heading("Set up Opal C1"));
        ui.add_space(12.0);
        if phase == Phase::Checking {
            ui.vertical_centered(|ui| ui.label("Checking the camera files..."));
            return;
        }
        ui.label(
            "The camera needs a few files from Opal's own app, Opal Composer: its firmware and the networks it runs. \
             They are copied out of the app's disk image on this computer; nothing is sent anywhere.",
        );
        ui.add_space(12.0);
        ui.label(RichText::new("1. Download Opal Composer 2.0 from Opal's site.").strong());
        if ui.button("Open Opal's download page").clicked() {
            open_page();
        }
        ui.add_space(12.0);
        ui.label(RichText::new(format!("2. Choose the file you downloaded, {DMG_NAME}.")).strong());
        let busy = phase == Phase::Extracting;
        ui.add_enabled_ui(!busy, |ui| {
            if self.can_browse && ui.button("Choose the file").clicked() {
                let (path, phase, progress, ctx) = (
                    self.path.clone(),
                    self.phase.clone(),
                    self.progress.clone(),
                    ui.ctx().clone(),
                );
                std::thread::spawn(move || {
                    let mut dialog = file_dialog("Choose the Opal Composer disk image", "Disk images", &["dmg"]);
                    if let Ok(out) = dialog.output()
                        && out.status.success()
                    {
                        let picked = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                        if !picked.is_empty() {
                            *path.lock().unwrap() = picked.clone();
                            extract(picked, phase, progress, ctx);
                        }
                    }
                });
            }
            ui.horizontal(|ui| {
                let mut text = self.path.lock().unwrap().clone();
                let field = ui.add(
                    egui::TextEdit::singleline(&mut text)
                        .hint_text("or type the path to the disk image")
                        .desired_width(ui.available_width() - 90.0),
                );
                if field.changed() {
                    *self.path.lock().unwrap() = text.clone();
                }
                if ui
                    .add_enabled(!text.trim().is_empty(), egui::Button::new("Use it"))
                    .clicked()
                {
                    extract(
                        text.trim().to_owned(),
                        self.phase.clone(),
                        self.progress.clone(),
                        ui.ctx().clone(),
                    );
                }
            });
        });
        ui.add_space(12.0);
        match phase {
            Phase::Extracting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Copying the camera files out of the disk image...");
                });
                for line in self.progress.lock().unwrap().iter() {
                    ui.label(RichText::new(line).color(WEAK).small());
                }
            }
            Phase::Failed(why) => {
                ui.label(RichText::new(why).color(RED));
            }
            _ => {}
        }
    }
}

/// Unpacks the camera files from `dmg`, then starts the service.
fn extract(dmg: String, phase: Arc<Mutex<Phase>>, progress: Arc<Mutex<Vec<String>>>, ctx: egui::Context) {
    *phase.lock().unwrap() = Phase::Extracting;
    progress.lock().unwrap().clear();
    ctx.request_repaint();
    std::thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let mut child = tool(&["assets", "extract", &dmg])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("cannot run opal-rs: {e}"))?;
            let mut unknown = false;
            if let Some(out) = child.stdout.take() {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    unknown |= line.contains("unknown disk image");
                    progress.lock().unwrap().push(line);
                    ctx.request_repaint();
                }
            }
            let mut err = String::new();
            if let Some(e) = child.stderr.take() {
                for line in BufReader::new(e).lines().map_while(Result::ok) {
                    err = line;
                }
            }
            let ok = child.wait().is_ok_and(|s| s.success());
            if unknown {
                return Err(format!(
                    "That file is not Opal Composer 2.0 (build 24). The one you need is {DMG_NAME}."
                ));
            }
            if !ok {
                return Err(if err.is_empty() {
                    "The camera files could not be copied.".into()
                } else {
                    err
                });
            }
            // The panel itself says when the service is not running.
            if let Err(e) = opal_ipc::start_service() {
                eprintln!("opal-rs service: {e}");
            }
            Ok(())
        })();
        *phase.lock().unwrap() = match result {
            Ok(()) => Phase::Ready,
            Err(e) => Phase::Failed(e),
        };
        ctx.request_repaint();
    });
}

/// Opens Opal's download page in the browser.
fn open_page() {
    #[cfg(unix)]
    let r = Command::new("xdg-open").arg(OPAL_PAGE).spawn();
    #[cfg(windows)]
    let r = {
        let mut c = Command::new("cmd");
        c.args(["/c", "start", "", OPAL_PAGE]);
        std::os::windows::process::CommandExt::creation_flags(&mut c, opal_ipc::NO_WINDOW);
        c.spawn()
    };
    match r {
        // Collected when it exits, so it does not linger as a zombie.
        Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
        Err(e) => eprintln!("{OPAL_PAGE}: {e}"),
    }
}
