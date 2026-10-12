//! The Windows install and uninstall steps.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW,
    RegDeleteKeyValueW, RegDeleteTreeW, RegSetValueExW,
};
use windows::Win32::UI::Shell::{IShellLinkW, IsUserAnAdmin, ShellExecuteW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_YESNO, MessageBoxW, SW_SHOWNORMAL,
};
use windows::core::{HSTRING, Interface, PCWSTR};

const TITLE: &str = "Opal C1 setup";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const UNINSTALL_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\opal-rs";
const SETUP: &str = "opal-rs-setup.exe";
/// What is installed, in this order.
const FILES: [&str; 4] = ["opal-rs.exe", "opal-rs-gui.exe", "opal-rs-tray.exe", "opal_vcam.dll"];

/// The programs, when built into the installer.
#[cfg(feature = "payload")]
fn payload(name: &str) -> Option<&'static [u8]> {
    macro_rules! file {
        ($n:literal) => {
            include_bytes!(concat!(env!("OPAL_RS_PAYLOAD"), "/", $n)).as_slice()
        };
    }
    Some(match name {
        "opal-rs.exe" => file!("opal-rs.exe"),
        "opal-rs-gui.exe" => file!("opal-rs-gui.exe"),
        "opal-rs-tray.exe" => file!("opal-rs-tray.exe"),
        "opal_vcam.dll" => file!("opal_vcam.dll"),
        _ => return None,
    })
}

#[cfg(not(feature = "payload"))]
fn payload(_: &str) -> Option<&'static [u8]> {
    None
}

fn message(text: &str, style: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE) -> bool {
    unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(TITLE), style) == IDYES }
}

fn install_dir() -> PathBuf {
    std::env::var_os("ProgramFiles")
        .map_or_else(|| PathBuf::from("C:\\Program Files"), PathBuf::from)
        .join("opal-rs")
}

fn shortcut() -> PathBuf {
    std::env::var_os("ProgramData")
        .map_or_else(|| PathBuf::from("C:\\ProgramData"), PathBuf::from)
        .join("Microsoft\\Windows\\Start Menu\\Programs\\Opal C1 Control.lnk")
}

pub fn main() {
    let uninstall = std::env::args().nth(1).as_deref() == Some("uninstall");
    if !unsafe { IsUserAnAdmin() }.as_bool() {
        // Asks for administrator rights and runs again.
        let exe = std::env::current_exe().unwrap_or_default();
        let args = if uninstall { "uninstall" } else { "" };
        unsafe {
            ShellExecuteW(
                None,
                &HSTRING::from("runas"),
                &HSTRING::from(exe.as_os_str()),
                &HSTRING::from(args),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
        return;
    }
    let result = if uninstall { remove() } else { install() };
    if let Err(e) = result {
        message(&e, MB_OK | MB_ICONERROR);
    }
}

fn install() -> Result<(), String> {
    let dir = install_dir();
    let source = std::env::current_exe().map_err(|e| e.to_string())?;
    let source_dir = source.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    stop_running();
    for name in FILES {
        let to = dir.join(name);
        let written = match payload(name) {
            Some(bytes) => std::fs::write(&to, bytes),
            None => std::fs::copy(source_dir.join(name), &to).map(drop),
        };
        if let Err(e) = written {
            return Err(if name == "opal_vcam.dll" {
                format!(
                    "Cannot replace {}: {e}\n\nClose any app that is using the camera, then run setup again.",
                    to.display()
                )
            } else {
                format!("Cannot write {}: {e}", to.display())
            });
        }
    }
    if source != dir.join(SETUP) {
        std::fs::copy(&source, dir.join(SETUP)).map_err(|e| format!("Cannot copy the installer: {e}"))?;
    }
    let dll = dir.join("opal_vcam.dll");
    let code = Command::new("regsvr32")
        .arg("/s")
        .arg(&dll)
        .status()
        .map_err(|e| format!("regsvr32: {e}"))?;
    if !code.success() {
        return Err(format!(
            "Registering the virtual camera failed (regsvr32 exit code {}).",
            code.code().unwrap_or(-1)
        ));
    }
    let tray = dir.join("opal-rs-tray.exe");
    set_string(RUN_KEY, "opal-rs", &format!("\"{}\"", tray.display()))?;
    set_string(UNINSTALL_KEY, "DisplayName", "Opal C1 (opal-rs)")?;
    set_string(UNINSTALL_KEY, "DisplayVersion", env!("CARGO_PKG_VERSION"))?;
    set_string(UNINSTALL_KEY, "Publisher", "opal-rs")?;
    set_string(UNINSTALL_KEY, "InstallLocation", &dir.display().to_string())?;
    set_string(
        UNINSTALL_KEY,
        "DisplayIcon",
        &dir.join("opal-rs-gui.exe").display().to_string(),
    )?;
    set_string(
        UNINSTALL_KEY,
        "UninstallString",
        &format!("\"{}\" uninstall", dir.join(SETUP).display()),
    )?;
    set_dword(UNINSTALL_KEY, "NoModify", 1)?;
    set_dword(UNINSTALL_KEY, "NoRepair", 1)?;
    if let Err(e) = make_shortcut(&dir.join("opal-rs-gui.exe"), &shortcut()) {
        eprintln!("Start menu entry: {e}");
    }
    let start = message(
        "Opal C1 is installed. It starts when you sign in.\n\nStart it now? The control panel will ask for the \
         Opal Composer disk image the first time.",
        MB_YESNO | MB_ICONINFORMATION,
    );
    if start {
        // Explorer starts them as the signed-in user rather than as administrator.
        for program in ["opal-rs-tray.exe", "opal-rs-gui.exe"] {
            let _ = Command::new("explorer.exe").arg(dir.join(program)).spawn();
        }
    }
    Ok(())
}

fn remove() -> Result<(), String> {
    let dir = install_dir();
    stop_running();
    let _ = Command::new("regsvr32")
        .args(["/u", "/s"])
        .arg(dir.join("opal_vcam.dll"))
        .status();
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_LOCAL_MACHINE, &HSTRING::from(RUN_KEY), &HSTRING::from("opal-rs"));
        let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(UNINSTALL_KEY));
    }
    let _ = std::fs::remove_file(shortcut());
    for name in FILES {
        let path = dir.join(name);
        if std::fs::remove_file(&path).is_err() {
            delete_at_restart(&path);
        }
    }
    // The installer is running from the folder, so it and the folder go at the next restart.
    delete_at_restart(&dir.join(SETUP));
    delete_at_restart(&dir);
    let data: Vec<PathBuf> = ["APPDATA", "LOCALAPPDATA"]
        .iter()
        .filter_map(|v| Some(PathBuf::from(std::env::var_os(v)?).join("opal-rs")))
        .filter(|d| d.exists())
        .collect();
    if !data.is_empty()
        && message(
            "Opal C1 is removed.\n\nAlso delete your settings, presets, stickers and the camera files copied from \
             Opal Composer?",
            MB_YESNO | MB_ICONINFORMATION,
        )
    {
        for d in data {
            if std::fs::remove_dir_all(&d).is_err() {
                delete_at_restart(&d);
            }
        }
    }
    Ok(())
}

/// Asks a running service to quit, then ends the tray, the panel and anything left.
fn stop_running() {
    if opal_ipc::request("quit", Duration::from_secs(3)).is_ok() {
        let start = Instant::now();
        while opal_ipc::request("status", Duration::from_secs(1)).is_ok() && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    for name in ["opal-rs-tray.exe", "opal-rs-gui.exe", "opal-rs.exe"] {
        let mut c = Command::new("taskkill");
        c.args(["/F", "/IM", name]);
        std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000);
        let _ = c.output();
    }
}

fn delete_at_restart(path: &Path) {
    unsafe {
        let _ = MoveFileExW(
            &HSTRING::from(path.as_os_str()),
            PCWSTR::null(),
            MOVEFILE_DELAY_UNTIL_REBOOT,
        );
    }
}

fn open_key(key: &str) -> Result<HKEY, String> {
    let mut hkey = HKEY::default();
    let r = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(key),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
    };
    if r != ERROR_SUCCESS {
        return Err(format!("Cannot open the registry key {key} ({})", r.0));
    }
    Ok(hkey)
}

fn set_string(key: &str, name: &str, value: &str) -> Result<(), String> {
    let hkey = open_key(key)?;
    let data: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let bytes = unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2) };
    let r = unsafe { RegSetValueExW(hkey, &HSTRING::from(name), None, REG_SZ, Some(bytes)) };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if r != ERROR_SUCCESS {
        return Err(format!("Cannot write {key}\\{name} ({})", r.0));
    }
    Ok(())
}

fn set_dword(key: &str, name: &str, value: u32) -> Result<(), String> {
    let hkey = open_key(key)?;
    let r = unsafe { RegSetValueExW(hkey, &HSTRING::from(name), None, REG_DWORD, Some(&value.to_le_bytes())) };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if r != ERROR_SUCCESS {
        return Err(format!("Cannot write {key}\\{name} ({})", r.0));
    }
    Ok(())
}

/// A Start menu shortcut to `target`.
fn make_shortcut(target: &Path, at: &Path) -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(target.as_os_str()))?;
        if let Some(dir) = target.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        }
        link.SetDescription(&HSTRING::from("Control panel for the Opal C1 camera"))?;
        link.cast::<IPersistFile>()?.Save(&HSTRING::from(at.as_os_str()), true)
    }
}
