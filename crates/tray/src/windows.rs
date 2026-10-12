//! The tray icon in the Windows notification area, with the same menu as on Linux.

use std::time::{Duration, Instant};

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage};

use crate::{MODES, Tray, dot, open_panel, program, send};

/// Starts the service without a window, logging to `%LOCALAPPDATA%\opal-rs\service.log`.
fn start_service() {
    use std::os::windows::process::CommandExt;
    let dir = std::env::var_os("LOCALAPPDATA")
        .map_or_else(std::env::temp_dir, std::path::PathBuf::from)
        .join("opal-rs");
    let _ = std::fs::create_dir_all(&dir);
    let log = std::fs::File::create(dir.join("service.log"));
    let mut c = std::process::Command::new(program("opal-rs"));
    c.arg("run").creation_flags(0x0800_0000);
    if let Ok(log) = log
        && let Ok(err) = log.try_clone()
    {
        c.stdout(log).stderr(err);
    }
    if let Err(e) = c.spawn() {
        eprintln!("opal-rs: {e}");
    }
}

fn icon(colour: [u8; 3]) -> Icon {
    let size = 32;
    let rgba = dot(size, colour)
        .into_iter()
        .flat_map(|(a, [r, g, b])| [r, g, b, a])
        .collect();
    Icon::from_rgba(rgba, size as u32, size as u32).expect("icon size matches its pixels")
}

pub fn run() {
    if send("status").is_none() {
        start_service();
    }
    let summary = MenuItem::new("", false, None);
    let video = CheckMenuItem::new("Video on", false, true, None);
    let modes = MODES.map(|(label, _)| CheckMenuItem::new(label, false, false, None));
    let panel = MenuItem::new("Open control panel", true, None);
    let hide = MenuItem::new("Hide this icon", true, None);
    let menu = Menu::new();
    let built = menu.append_items(&[
        &summary,
        &PredefinedMenuItem::separator(),
        &video,
        &modes[0],
        &modes[1],
        &modes[2],
        &PredefinedMenuItem::separator(),
        &panel,
        &hide,
    ]);
    let mut tray = Tray::default();
    let icon_tray = built.ok().and_then(|_| {
        TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Opal C1")
            .with_icon(icon(tray.colour()))
            .build()
            .ok()
    });
    let Some(icon_tray) = icon_tray else {
        eprintln!("no notification area available");
        std::process::exit(1);
    };
    let mut last_poll = Instant::now() - Duration::from_secs(10);
    let mut shown = None;
    loop {
        // The tray's window lives on this thread, so its messages are handled here.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let mut changed = false;
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == video.id() {
                send(if tray.video_off() { "video on" } else { "video off" });
            } else if let Some(i) = modes.iter().position(|m| event.id == *m.id()) {
                send(MODES[i].1);
            } else if event.id == panel.id() {
                open_panel();
            } else if event.id == hide.id() {
                return;
            }
            changed = true;
        }
        if changed || last_poll.elapsed() >= Duration::from_secs(2) {
            last_poll = Instant::now();
            tray.status = send("status").unwrap_or_default();
            let state = (
                tray.summary(),
                tray.colour(),
                tray.video_off(),
                tray.mode(),
                tray.running(),
            );
            if shown.as_ref() != Some(&state) {
                let _ = icon_tray.set_icon(Some(icon(tray.colour())));
                let _ = icon_tray.set_tooltip(Some(format!("Opal C1: {}", tray.summary())));
                summary.set_text(tray.summary());
                video.set_enabled(tray.running());
                video.set_checked(!tray.video_off());
                for (i, m) in modes.iter().enumerate() {
                    m.set_enabled(tray.running());
                    m.set_checked(tray.running() && tray.mode() == i);
                }
                shown = Some(state);
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}
