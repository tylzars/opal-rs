//! The Virtual cameras card (Linux): Opal C1 Virtual and extra cameras, one for each app that should use the
//! Opal C1 at the same time. Adding or removing one runs `opal-rs cameras`, which asks for the administrator
//! password.

use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Ui, vec2};

use crate::theme::{TEXT, WEAK, card, hint};

#[derive(Default)]
pub struct Card {
    name: String,
    /// Set while a change runs; then its outcome.
    job: Arc<Mutex<Job>>,
    /// The list was read again after the last change.
    refreshed: bool,
}

#[derive(Default)]
enum Job {
    #[default]
    Idle,
    Running(String),
    Done(String),
}

/// One camera from the service's `cameras` reply.
struct Row<'a> {
    name: &'a str,
    note: String,
    removable: bool,
    /// Added or removed, waiting for the cameras to be free.
    pending: bool,
}

fn rows(reply: &str) -> Vec<Row<'_>> {
    if reply.is_empty() || reply.starts_with("error") {
        return Vec::new();
    }
    reply
        .split(" | ")
        .enumerate()
        .filter_map(|(i, line)| {
            let mut w = line.splitn(4, ' ');
            let (_nr, state, apps, name) = (w.next()?, w.next()?, w.next()?, w.next()?);
            let note = match (state, apps) {
                ("restart", _) => "Appears after the computer restarts".to_owned(),
                ("leaving", _) => "Removed: goes away when the computer restarts".to_owned(),
                ("on", "-") => "Free".to_owned(),
                ("on", apps) => format!("Used by {}", apps.replace(',', ", ")),
                _ => "Not in use".to_owned(),
            };
            Some(Row {
                name,
                note,
                removable: i > 0 && state != "leaving",
                pending: matches!(state, "restart" | "leaving"),
            })
        })
        .collect()
}

impl Card {
    /// `reply` is the service's latest `cameras` reply. Returns true when the list should be read again.
    pub fn show(&mut self, ui: &mut Ui, reply: &str) -> bool {
        let running = matches!(*self.job.lock().unwrap(), Job::Running(_));
        card(ui, "", |ui| {
            hint(
                ui,
                "Linux lets one app use a camera at a time. To use the Opal C1 in two apps at once, add a camera for \
                 the second app here and pick it in that app.",
            );
            ui.add_space(4.0);
            let rows = rows(reply);
            let pending = rows.iter().any(|r| r.pending);
            if rows.is_empty() {
                ui.label(egui::RichText::new("The camera service is not running.").color(WEAK));
            }
            for row in rows {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(row.name).color(TEXT));
                        ui.label(egui::RichText::new(&row.note).small().color(WEAK));
                    });
                    if row.removable {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add_enabled(!running, egui::Button::new("Remove")).clicked() {
                                self.start("remove", Some(row.name));
                            }
                        });
                    }
                });
            }
            if pending {
                ui.add_space(4.0);
                hint(
                    ui,
                    "Some changes wait until no app uses an Opal camera, or until the computer restarts.",
                );
                if ui.add_enabled(!running, egui::Button::new("Apply now")).clicked() {
                    self.start("apply", None);
                }
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let edit = egui::TextEdit::singleline(&mut self.name)
                    .hint_text("Name, like Zoom")
                    .char_limit(31)
                    .desired_width(ui.available_width() - 110.0)
                    .margin(vec2(10.0, 7.0));
                let r = ui.add(edit);
                let name = self.name.trim().to_owned();
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let ok = !name.is_empty() && !running;
                if (ui.add_enabled(ok, egui::Button::new("Add camera")).clicked() || enter) && ok {
                    self.start("add", Some(&name));
                    self.name.clear();
                }
            });
            match &*self.job.lock().unwrap() {
                Job::Idle => {}
                Job::Running(what) => {
                    ui.label(egui::RichText::new(what).small().color(WEAK));
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                }
                Job::Done(text) => {
                    ui.label(egui::RichText::new(text).small().color(TEXT));
                }
            }
        });
        let done = matches!(*self.job.lock().unwrap(), Job::Done(_));
        let refresh = done && !self.refreshed;
        self.refreshed |= done;
        refresh
    }

    /// Runs `opal-rs cameras <verb> [name]` in the background; pkexec asks for the password.
    fn start(&mut self, verb: &'static str, name: Option<&str>) {
        self.refreshed = false;
        let doing = match (verb, name) {
            ("add", Some(n)) => format!("Adding {n}"),
            ("remove", Some(n)) => format!("Removing {n}"),
            _ => "Applying".to_owned(),
        };
        *self.job.lock().unwrap() = Job::Running(format!("{doing}: enter the administrator password..."));
        let (job, name) = (self.job.clone(), name.map(str::to_owned));
        std::thread::spawn(move || {
            let out = Command::new(opal_ipc::program("opal-rs"))
                .args(["cameras", verb])
                .args(name)
                .stdin(Stdio::null())
                .output();
            let text = match out {
                Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_owned(),
                // pkexec: the password prompt was closed or refused.
                Ok(o) if matches!(o.status.code(), Some(126 | 127)) => "Nothing changed: no password given.".into(),
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr);
                    let last = err.lines().last().unwrap_or("failed").trim();
                    last.strip_prefix("Error: ").unwrap_or(last).to_owned()
                }
                Err(e) => format!("opal-rs: {e}"),
            };
            *job.lock().unwrap() = Job::Done(text);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_service_reply() {
        let r = rows("10 on firefox,zoom Opal C1 Virtual | 11 on - Zoom (work) | 12 restart - Spare cam");
        assert_eq!(r.len(), 3);
        assert!(!r[0].removable && r[0].name == "Opal C1 Virtual" && r[0].note == "Used by firefox, zoom");
        assert!(r[1].removable && r[1].name == "Zoom (work)" && r[1].note == "Free");
        assert_eq!(r[2].note, "Appears after the computer restarts");
        assert!(r[2].pending && !r[1].pending);
        assert!(rows("").is_empty() && rows("error: unknown command").is_empty());
    }
}
