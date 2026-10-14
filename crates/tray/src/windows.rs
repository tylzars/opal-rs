//! The tray icon in the Windows notification area, with the same menu as on Linux.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage};

use crate::{MODES, Tray, dot, open_panel, send, toggle_autostart};

fn icon(colour: [u8; 3]) -> Icon {
    let size = 32;
    let rgba = dot(size, colour)
        .into_iter()
        .flat_map(|(a, [r, g, b])| [r, g, b, a])
        .collect();
    Icon::from_rgba(rgba, size as u32, size as u32).expect("icon size matches its pixels")
}

pub fn run() {
    if std::env::args().any(|a| a == opal_ipc::autostart::SIGN_IN) && !opal_ipc::autostart::enabled() {
        return;
    }
    // One icon per sign-in: the Run entry starts a second copy after the sign-in task has started one.
    let name: Vec<u16> = "Local\\opal-rs-tray".encode_utf16().chain([0]).collect();
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if mutex.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
    }
    if let Err(e) = opal_ipc::start_service() {
        eprintln!("opal-rs: {e}");
    }
    let summary = MenuItem::new("", false, None);
    let video = CheckMenuItem::new("Video on", false, true, None);
    let modes = MODES.map(|(label, _)| CheckMenuItem::new(label, false, false, None));
    let panel = MenuItem::new("Open control panel", true, None);
    let autostart = CheckMenuItem::new("Start at sign-in", true, opal_ipc::autostart::enabled(), None);
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
        &autostart,
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
    // The service is asked from another thread, so a slow or stopped one never holds up the menu.
    let latest: Arc<Mutex<Option<String>>> = Arc::default();
    let poller = {
        let latest = latest.clone();
        std::thread::spawn(move || {
            loop {
                let status = send("status").unwrap_or_default();
                *latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(status);
                std::thread::park_timeout(Duration::from_secs(2));
            }
        })
    };
    // A command, then a fresh status, both off this thread.
    let command = |c: &'static str| {
        let poller = poller.thread().clone();
        std::thread::spawn(move || {
            send(c);
            poller.unpark();
        });
    };
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
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == video.id() {
                command(if tray.video_off() { "video on" } else { "video off" });
            } else if let Some(i) = modes.iter().position(|m| event.id == *m.id()) {
                command(MODES[i].1);
            } else if event.id == panel.id() {
                open_panel();
            } else if event.id == autostart.id() {
                toggle_autostart();
                autostart.set_checked(opal_ipc::autostart::enabled());
            } else if event.id == hide.id() {
                return;
            }
            // A clicked item toggles its own tick; redraw so it shows the real state.
            shown = None;
        }
        let status = latest.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(status) = status {
            tray.status = status;
        }
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
        std::thread::sleep(Duration::from_millis(30));
    }
}
