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
        if app.status_value("video") == Some("off") {
            ui.horizontal(|ui| {
                ui.label(if app.c.gestures && app.c.peace {
                    "Video is off. Make a peace sign to turn it back on, or"
                } else {
                    "Video is off."
                });
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
    let mut note: Option<&str> = None;
    let message = &mut note;
    let c = &mut app.c;

    ui.heading("Camera");
    if ui.checkbox(&mut c.low_power, "Low power mode").changed() {
        cmds.push(format!("power {}", if c.low_power { "low" } else { "normal" }));
    }
    ui.weak(if cfg!(unix) {
        "Apps use the camera directly at 1080p, without effects. Restarts the camera."
    } else {
        "Plain 1080p, no networks on the camera, no effects. Restarts the camera."
    });
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

        let picture = ui
            .horizontal(|ui| {
                ui.label("Background picture");
                ui.text_edit_singleline(&mut c.background)
            })
            .inner;
        let entered = picture.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let toggled = ui.checkbox(&mut c.background_on, "Replace background").changed();
        if !c.background_on {
            if toggled {
                cmds.push("background off".into());
            }
        } else if toggled || entered {
            if c.background.trim().is_empty() {
                // Off here, so off in the service too.
                c.background_on = false;
                cmds.push("background off".into());
                *message = Some("Enter a picture file first.");
            } else {
                cmds.push(format!("background {}", c.background.trim()));
            }
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
        let mut send_look = false;
        if !opal_effects::LOOKS.contains(&c.look.as_str()) {
            let file = ui
                .horizontal(|ui| {
                    ui.label(".cube file");
                    ui.text_edit_singleline(&mut c.look)
                })
                .inner;
            send_look |= c.look_on && file.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        }
        ui.horizontal(|ui| {
            if ui.checkbox(&mut c.look_on, "Use look").changed() {
                if c.look_on {
                    send_look = true;
                } else {
                    cmds.push("lut off".into());
                }
            }
            let s = ui.add_enabled(
                c.look_on,
                egui::Slider::new(&mut c.look_amount, 0.0..=1.0).text("Amount"),
            );
            send_look |= s.drag_stopped() || (s.changed() && !s.dragged());
        });
        if send_look {
            if c.look.trim().is_empty() {
                c.look_on = false;
                cmds.push("lut off".into());
                *message = Some("Choose a look or enter a .cube file first.");
            } else {
                c.look = c.look.trim().to_owned();
                cmds.push(c.look_cmd());
            }
        }

        for (name, value) in crate::LIGHTING.iter().zip(c.lighting.iter_mut()) {
            let s = ui.add(egui::Slider::new(value, -100..=100).text(*name));
            if s.drag_stopped() || (s.changed() && !s.dragged()) {
                cmds.push(format!("{name} {value}"));
            }
        }
        for ((name, _), v) in opal_effects::COLOURS.iter().zip(c.colours.iter_mut()) {
            ui.horizontal(|ui| {
                ui.label(*name);
                let mut done = false;
                for x in v.iter_mut() {
                    let s = ui.add(egui::DragValue::new(x).range(-100..=100));
                    done |= s.drag_stopped() || (s.changed() && !s.dragged());
                }
                if done {
                    cmds.push(format!("colour {name} {} {} {}", v[0], v[1], v[2]));
                }
            });
        }
        if ui.button("Reset lighting and colours").clicked() {
            (c.lighting, c.colours) = ([0; 5], [[0; 3]; 8]);
            cmds.push("adjust reset".into());
        }
        let s = ui.add(egui::Slider::new(&mut c.touchup, 0..=100).text("Touch up"));
        if s.drag_stopped() || (s.changed() && !s.dragged()) {
            cmds.push(format!("touchup {}", c.touchup));
        }
        let a = ui.add(egui::Slider::new(&mut c.tone[0], 0..=100).text("Even skin tone"));
        let b = ui.add(egui::Slider::new(&mut c.tone[1], -100..=100).text("Skin warmth"));
        if a.drag_stopped() || b.drag_stopped() || ((a.changed() || b.changed()) && !a.dragged() && !b.dragged()) {
            cmds.push(format!("tone {} {}", c.tone[0], c.tone[1]));
        }
        let s = ui.add(egui::Slider::new(&mut c.pixelate, 0..=100).text("Pixelate"));
        if s.drag_stopped() || (s.changed() && !s.dragged()) {
            cmds.push(format!("pixelate {}", c.pixelate));
        }
        let s = ui.add(egui::Slider::new(&mut c.magnify, 1.0..=4.0).text("Zoom"));
        if s.drag_stopped() || (s.changed() && !s.dragged()) {
            cmds.push(if c.magnify > 1.0 {
                format!("magnify {:.1}", c.magnify)
            } else {
                "magnify off".into()
            });
        }
        if ui.checkbox(&mut c.zoom, "Face tracking").changed() {
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
            for (name, label, on) in [
                ("peace", "Peace sign: video off and on", &mut c.peace),
                ("pinch", "Pinch: face tracking on and off", &mut c.pinch),
            ] {
                if ui.checkbox(on, label).changed() {
                    cmds.push(format!("gesture {name} {}", if *on { "on" } else { "off" }));
                }
            }
        });
    });

    crate::view::run(app, cmds);
    if let Some(note) = note {
        app.message = note.into();
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
