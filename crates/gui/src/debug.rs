//! Plain debug panel: every control in one list, with the raw status line.

use eframe::egui;

use crate::{App, Mode, RESOLUTIONS};

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    egui::Panel::top("debug_bar").show(ui, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Back to normal mode").clicked() {
                app.debug = false;
                app.want_preview = true;
            }
            ui.weak("Debug mode");
        });
        ui.add_space(2.0);
    });
    egui::Panel::left("controls")
        .default_size(380.0)
        .min_size(320.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| controls(app, ui));
        });
    egui::CentralPanel::default().show(ui, |ui| {
        ui.label(&app.status);
        if !app.message.is_empty() {
            ui.colored_label(egui::Color32::LIGHT_RED, &app.message);
        }
        if app.status.ends_with("video off") {
            ui.horizontal(|ui| {
                ui.label("Video is off. Make a peace sign to turn it back on, or");
                if ui.button("Turn video back on").clicked() {
                    app.apply("video on".into());
                }
            });
        }
        ui.separator();
        preview(app, ui);
    });
}

fn controls(app: &mut App, ui: &mut egui::Ui) {
    let mut cmds: Vec<String> = Vec::new();
    let c = &mut app.c;

    ui.heading("Camera");
    if ui.checkbox(&mut c.low_power, "Low power mode").changed() {
        cmds.push(format!("power {}", if c.low_power { "low" } else { "normal" }));
    }
    ui.weak("Apps use the camera directly at 1080p, without effects. Restarts the camera.");
    ui.add_enabled_ui(!c.low_power, |ui| {
        ui.horizontal(|ui| {
            ui.label("Resolution");
            egui::ComboBox::from_id_salt("resolution")
                .selected_text(&c.resolution)
                .show_ui(ui, |ui| {
                    for (name, _, _) in RESOLUTIONS {
                        if ui.selectable_value(&mut c.resolution, name.to_owned(), name).changed() {
                            cmds.push(format!("resolution {name}"));
                        }
                    }
                });
            ui.weak("restarts the camera");
        })
    });
    ui.horizontal(|ui| {
        ui.label("Flicker");
        for (v, name) in [("auto", "Auto"), ("50", "50 Hz"), ("60", "60 Hz"), ("off", "Off")] {
            if ui.radio_value(&mut c.antibanding, v.to_owned(), name).changed() {
                cmds.push(format!("antibanding {v}"));
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("Streaming");
        for (m, name) in [(Mode::Auto, "Auto"), (Mode::On, "On"), (Mode::Off, "Off")] {
            if ui.radio_value(&mut app.mode, m, name).changed() {
                cmds.push(name.to_lowercase());
            }
        }
    });

    if ui.checkbox(&mut c.auto_focus, "Autofocus").changed() {
        cmds.push(if c.auto_focus {
            "af".into()
        } else {
            format!("focus {}", c.focus)
        });
    }
    if ui
        .add_enabled(!c.auto_focus, egui::Slider::new(&mut c.focus, 0..=255).text("Focus"))
        .changed()
    {
        cmds.push(format!("focus {}", c.focus));
    }

    if ui.checkbox(&mut c.auto_exposure, "Auto exposure").changed() {
        cmds.push(if c.auto_exposure {
            "ae".into()
        } else {
            format!("exposure {} {}", c.exposure_us, c.iso)
        });
    }
    ui.add_enabled_ui(!c.auto_exposure, |ui| {
        let a = ui.add(
            egui::Slider::new(&mut c.exposure_us, 100..=33000)
                .logarithmic(true)
                .text("Exposure (us)"),
        );
        let b = ui.add(egui::Slider::new(&mut c.iso, 100..=1600).text("ISO"));
        if a.changed() || b.changed() {
            cmds.push(format!("exposure {} {}", c.exposure_us, c.iso));
        }
    });
    if ui
        .add_enabled(
            c.auto_exposure,
            egui::Slider::new(&mut c.ev, -9..=9).text("Exposure compensation"),
        )
        .changed()
    {
        cmds.push(format!("ev {}", c.ev));
    }

    if ui.checkbox(&mut c.auto_wb, "Auto white balance").changed() {
        cmds.push(if c.auto_wb {
            "awb".into()
        } else {
            format!("wb {}", c.wb)
        });
    }
    if ui
        .add_enabled(
            !c.auto_wb,
            egui::Slider::new(&mut c.wb, 1000..=12000).text("White balance (K)"),
        )
        .changed()
    {
        cmds.push(format!("wb {}", c.wb));
    }

    for (name, value, range) in [
        ("brightness", &mut c.brightness, -10..=10),
        ("contrast", &mut c.contrast, -10..=10),
        ("saturation", &mut c.saturation, -10..=10),
        ("sharpness", &mut c.sharpness, 0..=4),
    ] {
        let label = format!("{}{}", name[..1].to_uppercase(), &name[1..]);
        if ui.add(egui::Slider::new(value, range).text(label)).changed() {
            cmds.push(format!("{name} {value}"));
        }
    }

    ui.separator();
    ui.heading("Microphone");
    if ui.checkbox(&mut c.mic, "Microphone on").changed() {
        cmds.push(format!("mic {}", if c.mic { "on" } else { "off" }));
    }
    if ui
        .add_enabled(c.mic, egui::Slider::new(&mut c.gain, 0.0..=30.0).text("Gain (dB)"))
        .changed()
    {
        cmds.push(format!("gain {:.1}", c.gain));
    }

    ui.separator();
    ui.heading("Effects");
    if c.low_power {
        ui.weak("Not available in low power mode.");
    }
    ui.add_enabled_ui(!c.low_power, |ui| {
        if ui.checkbox(&mut c.blur, "Blur background").changed() {
            cmds.push(c.blur_cmd());
        }
        if ui
            .add_enabled(c.blur, egui::Slider::new(&mut c.blur_radius, 4..=64).text("Blur"))
            .changed()
        {
            cmds.push(format!("blur {}", c.blur_radius));
        }

        ui.horizontal(|ui| {
            ui.label("Background picture");
            ui.text_edit_singleline(&mut c.background);
        });
        if ui.checkbox(&mut c.background_on, "Replace background").changed() {
            cmds.push(if c.background_on {
                format!("background {}", c.background)
            } else {
                "background off".into()
            });
        }

        ui.horizontal(|ui| {
            ui.label("Look");
            let builtin = opal_effects::LOOKS.contains(&c.look.as_str());
            egui::ComboBox::from_id_salt("look")
                .selected_text(if builtin { c.look.as_str() } else { "from file" })
                .show_ui(ui, |ui| {
                    for name in opal_effects::LOOKS {
                        if ui.selectable_label(c.look == name, name).clicked() {
                            c.look = name.to_owned();
                            if c.look_on {
                                cmds.push(c.look_cmd());
                            }
                        }
                    }
                    if ui.selectable_label(!builtin, "from file").clicked() && builtin {
                        c.look.clear();
                    }
                });
        });
        if !opal_effects::LOOKS.contains(&c.look.as_str()) {
            ui.horizontal(|ui| {
                ui.label(".cube file");
                ui.text_edit_singleline(&mut c.look);
            });
        }
        ui.horizontal(|ui| {
            if ui.checkbox(&mut c.look_on, "Use look").changed() {
                cmds.push(if c.look_on { c.look_cmd() } else { "lut off".into() });
            }
            let s = ui.add_enabled(
                c.look_on,
                egui::Slider::new(&mut c.look_amount, 0.0..=1.0).text("Amount"),
            );
            if s.drag_stopped() || (s.changed() && !s.dragged()) {
                cmds.push(c.look_cmd());
            }
        });

        if ui.checkbox(&mut c.zoom, "Follow my face").changed() {
            cmds.push(c.zoom_cmd());
        }
        if ui
            .add_enabled(c.zoom, egui::Slider::new(&mut c.zoom_amount, 0.0..=1.0).text("Framing"))
            .changed()
        {
            cmds.push(c.zoom_cmd());
        }
        ui.add_enabled_ui(c.zoom, |ui| {
            ui.horizontal(|ui| {
                ui.label("Tracking");
                for speed in ["slow", "normal", "fast"] {
                    if ui.radio_value(&mut c.tracking, speed.to_owned(), speed).changed() {
                        cmds.push(format!("tracking {speed}"));
                    }
                }
            });
        });

        if ui.checkbox(&mut c.gestures, "Hand gestures").changed() {
            cmds.push(c.gestures_cmd());
        }
        ui.add_enabled_ui(c.gestures, |ui| {
            ui.label("Peace sign: video off and on");
            ui.label("Pinch: follow my face on and off");
        });
    });

    for cmd in cmds {
        app.apply(cmd);
    }
}

fn preview(app: &mut App, ui: &mut egui::Ui) {
    let mut running = app.preview.running();
    if ui.checkbox(&mut running, "Preview").changed() {
        app.want_preview = running;
        if running {
            app.start_preview(ui.ctx());
        } else {
            app.preview.stop();
        }
    }
    if let Some(e) = app.preview.take_error() {
        app.message = e;
        app.want_preview = false;
    }
    app.update_texture(ui.ctx());
    if let (Some(t), true) = (&app.texture, running) {
        let w = ui.available_width();
        ui.add(egui::Image::new(t).fit_to_exact_size(egui::vec2(w, w * 9.0 / 16.0)));
    }
}
