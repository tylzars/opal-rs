//! The Windows install and uninstall steps.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, GetLastError};
use windows::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_DWORD, REG_MULTI_SZ, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_MULTI_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
};
use windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
use windows::Win32::System::Threading::{CreateMutexW, GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{
    FOLDERID_CommonPrograms, FOLDERID_ProgramFiles, IShellLinkW, IsUserAnAdmin, KF_FLAG_DEFAULT,
    SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, SHGetKnownFolderPath, ShellExecuteExW, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_YESNO, MessageBoxW, SW_SHOWNORMAL,
};
use windows::core::{HSTRING, Interface, PCWSTR};

const TITLE: &str = "Opal C1 setup";
const UNINSTALL_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\opal-rs";
const SETUP: &str = "opal-rs-setup.exe";
/// A program that was just stopped holds its file for a moment, so a busy file is tried again for this long.
const BUSY_WAIT: Duration = Duration::from_secs(5);
/// Windows' "file in use by another process".
const SHARING_VIOLATION: i32 = 32;
/// What is installed, in this order.
const FILES: [&str; 4] = ["opal-rs.exe", "opal-rs-gui.exe", "opal-rs-tray.exe", "opal_vcam.dll"];
/// Exit code of the administrator copy when an update needs a restart to finish (Windows' own code for it).
const RESTART_NEEDED: u32 = 3010;
/// The old virtual camera is still loaded; said even if setup fails afterwards.
static RESTART_FIRST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The scheduled task that starts the tray at sign-in.
const TASK: &str = "opal-rs";
/// The scheduled task that switches off the camera's own webcam, and the argument that has setup do it.
const CAMERA_TASK: &str = "opal-rs-camera";
const HIDE_CAMERA: &str = "hide-camera";

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

/// A folder as Windows knows it, not as the environment says: the administrator copy should not follow variables the
/// signed-in account can set.
fn known_folder(id: windows::core::GUID, fallback: &str) -> PathBuf {
    unsafe {
        match SHGetKnownFolderPath(&id, KF_FLAG_DEFAULT, None) {
            Ok(p) => {
                let path = p.to_string().map_or_else(|_| PathBuf::from(fallback), PathBuf::from);
                CoTaskMemFree(Some(p.0 as *const _));
                path
            }
            Err(_) => PathBuf::from(fallback),
        }
    }
}

fn install_dir() -> PathBuf {
    known_folder(FOLDERID_ProgramFiles, "C:\\Program Files").join("opal-rs")
}

fn shortcut() -> PathBuf {
    known_folder(
        FOLDERID_CommonPrograms,
        "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs",
    )
    .join("Opal C1 Control.lnk")
}

/// Tells the administrator copy that the signed-in user's copy deals with that user: their data, and starting the
/// programs as them.
const USER_ASKS: &str = "--user-asks";

pub fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == HIDE_CAMERA) {
        crate::camera::hide();
        return;
    }
    let uninstall = args.first().is_some_and(|a| a == "uninstall");
    let user_asks = args.iter().any(|a| a == USER_ASKS);
    // One setup at a time; the administrator copy this one starts is part of it.
    let name: Vec<u16> = "Local\\opal-rs-setup".encode_utf16().chain([0]).collect();
    let one = (!user_asks).then(|| unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) });
    // Denied too: one started as administrator owns it.
    let taken = one
        .as_ref()
        .is_some_and(|m| m.is_err() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS);
    if taken {
        message("Opal C1 setup is already running.", MB_OK | MB_ICONINFORMATION);
        return;
    }
    let fresh = !install_dir().join("opal-rs-tray.exe").exists();
    if !unsafe { IsUserAnAdmin() }.as_bool() {
        // Runs again as administrator, which can be another account, so this copy stops this user's service, and
        // afterwards asks about this user's data or starts the programs as this user.
        let was_running = quit_service();
        let args = if uninstall {
            format!("uninstall {USER_ASKS}")
        } else {
            USER_ASKS.to_owned()
        };
        let done = run_as_admin(&args);
        match (done, uninstall) {
            (Some(0), true) => {
                stop_autostart();
                remove_data();
            }
            (Some(0), false) => offer_start(&install_dir(), fresh, false),
            (Some(RESTART_NEEDED), false) => offer_start(&install_dir(), fresh, true),
            // Cancelled or failed: put back what was running.
            _ => {
                if was_running {
                    let _ = opal_ipc::start_service_with(&install_dir().join("opal-rs.exe"));
                }
                match done {
                    None => {
                        message(
                            "Setup needs permission to make changes, so nothing was changed.",
                            MB_OK | MB_ICONINFORMATION,
                        );
                    }
                    // 1: the administrator copy said what went wrong itself. Anything else: it stopped early.
                    Some(1) => {}
                    Some(_) => {
                        message("Setup stopped before it finished. Run it again.", MB_OK | MB_ICONERROR);
                    }
                }
            }
        }
        return;
    }
    let result = if uninstall {
        remove(!user_asks).map(|()| false)
    } else {
        install(!user_asks, fresh)
    };
    match result {
        Err(e) => {
            let restart = if RESTART_FIRST.load(std::sync::atomic::Ordering::Relaxed) {
                "\n\nRestart the computer before running setup again."
            } else {
                ""
            };
            message(&format!("{e}{restart}"), MB_OK | MB_ICONERROR);
            std::process::exit(1);
        }
        // Told to this account's copy, which says it.
        Ok(true) if user_asks => std::process::exit(RESTART_NEEDED as i32),
        Ok(_) => {}
    }
}

/// Runs this program again as administrator and waits for it. Its exit code (0 done, [`RESTART_NEEDED`], else
/// failed), or None if it did not run (permission refused).
fn run_as_admin(args: &str) -> Option<u32> {
    let exe = HSTRING::from(std::env::current_exe().unwrap_or_default().as_os_str());
    let (verb, params) = (HSTRING::from("runas"), HSTRING::from(args));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        if ShellExecuteExW(&mut info).is_err() || info.hProcess.is_invalid() {
            return None;
        }
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        Some(code)
    }
}

/// Installs; with `ask_start`, also offers to start the programs (when no user copy will). True when a restart is
/// needed to finish.
fn install(ask_start: bool, fresh: bool) -> Result<bool, String> {
    let dir = install_dir();
    let mut restart = false;
    let source = std::env::current_exe().map_err(|e| e.to_string())?;
    let source_dir = source.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    stop_running();
    for name in FILES {
        let to = dir.join(name);
        let write = || match payload(name) {
            Some(bytes) => std::fs::write(&to, bytes),
            None => std::fs::copy(source_dir.join(name), &to).map(drop),
        };
        let start = Instant::now();
        let mut written = loop {
            match write() {
                Err(e) if e.raw_os_error() == Some(SHARING_VIOLATION) && start.elapsed() < BUSY_WAIT => {
                    std::thread::sleep(Duration::from_millis(250))
                }
                r => break r,
            }
        };
        let busy = |w: &std::io::Result<()>| w.as_ref().is_err_and(|e| e.raw_os_error() == Some(SHARING_VIOLATION));
        // Windows' camera service keeps the old camera loaded. Stop it so the new one is used at once; it starts
        // again by itself when an app next opens a camera.
        if busy(&written) && name == "opal_vcam.dll" {
            stop_frame_server();
            let start = Instant::now();
            while busy(&written) && start.elapsed() < BUSY_WAIT {
                std::thread::sleep(Duration::from_millis(250));
                written = write();
            }
        }
        // Still loaded somewhere: a loaded file can be renamed. Move it aside, delete it at the next restart and
        // write the new one.
        if busy(&written) && name == "opal_vcam.dll" {
            let old = dir.join(format!("opal_vcam.{}.old", std::process::id()));
            if std::fs::rename(&to, &old).is_ok() {
                delete_at_restart(&old);
                written = write();
                // Windows' camera service keeps the old one until then.
                restart = true;
                RESTART_FIRST.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
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
    // An uninstall since the last restart may have left these files to be deleted then.
    keep_at_restart(&dir);
    let dll = dir.join("opal_vcam.dll");
    let code = Command::new(windows_program("System32\\regsvr32.exe"))
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
    // Without it the Run entry still starts the tray, only later.
    if let Err(e) = add_sign_in_task(&dir) {
        eprintln!("{e}");
    }
    if let Err(e) = add_camera_task(&dir) {
        eprintln!("{e}");
    }
    crate::camera::hide();
    if ask_start {
        offer_start(&dir, fresh, restart);
    }
    Ok(restart)
}

/// Says it is installed and offers to start the tray and the control panel, as whoever runs this. A `fresh` install
/// starts at sign-in for this account; an update keeps the account's choice.
fn offer_start(dir: &Path, fresh: bool, restart: bool) {
    if fresh && let Err(e) = opal_ipc::autostart::set(true, &dir.join("opal-rs-tray.exe")) {
        eprintln!("start at sign-in: {e}");
    }
    // The virtual camera is registered anyway, so it appears once Windows is upgraded.
    let windows10 = if opal_vcam::host::available() {
        ""
    } else {
        "\n\nopal-rs needs Windows 11. On this PC Opal C1 works as a plain webcam: in applications, choose \"Opal C1\"."
    };
    let first_run =
        std::env::var_os("LOCALAPPDATA").is_some_and(|d| !PathBuf::from(d).join("opal-rs\\assets").exists());
    let ask = if first_run {
        " It will ask for Opal Composer first."
    } else {
        ""
    };
    let at_sign_in = if opal_ipc::autostart::enabled() {
        " and starts at sign-in"
    } else {
        ""
    };
    let restart = if restart {
        "\n\nRestart the computer to finish updating Opal C1 Virtual; until then it may show no picture."
    } else {
        ""
    };
    let start = message(
        &format!("Opal C1 is installed{at_sign_in}.{windows10}{restart}\n\nStart it now?{ask}"),
        MB_YESNO | MB_ICONINFORMATION,
    );
    if start {
        // Explorer starts them unelevated, as the signed-in user, even when this copy runs as administrator.
        for program in ["opal-rs-tray.exe", "opal-rs-gui.exe"] {
            let _ = Command::new(windows_program("explorer.exe"))
                .arg(dir.join(program))
                .spawn();
        }
    }
}

/// Removes the programs; with `ask_data`, also offers to delete this account's settings and camera files.
fn remove(ask_data: bool) -> Result<(), String> {
    let dir = install_dir();
    stop_running();
    let _ = Command::new(windows_program("System32\\regsvr32.exe"))
        .args(["/u", "/s"])
        .arg(dir.join("opal_vcam.dll"))
        .status();
    unsafe {
        let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(UNINSTALL_KEY));
    }
    let _ = std::fs::remove_file(shortcut());
    delete_task(TASK);
    delete_task(CAMERA_TASK);
    crate::camera::unhide();
    // Every account's Start at sign-in, which would point at the removed tray.
    crate::accounts::clear_run_value(opal_ipc::autostart::RUN_NAME);
    for name in FILES {
        let path = dir.join(name);
        if std::fs::remove_file(&path).is_err() {
            delete_at_restart(&path);
        }
    }
    // The installer is running from the folder, so it and the folder go at the next restart.
    delete_at_restart(&dir.join(SETUP));
    delete_at_restart(&dir);
    if ask_data {
        stop_autostart();
        remove_data();
    }
    Ok(())
}

/// Windows starts its Run entries one at a time, each after the one before has settled, so the tray can start
/// minutes after sign-in. This task starts it at once, for every account; the tray leaves again when its account has
/// Start at sign-in off, and the Run entry's later copy leaves because one is running.
fn add_sign_in_task(dir: &Path) -> Result<(), String> {
    let tray = dir.join("opal-rs-tray.exe").display().to_string().replace('&', "&amp;");
    // Any account in Users, as itself; normal priority (a task's default is below normal); no time limit.
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Starts the Opal C1 tray icon at sign-in when Start at sign-in is on.</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled></LogonTrigger></Triggers>
  <Principals><Principal id="Users"><GroupId>S-1-5-32-545</GroupId><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>Parallel</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Users"><Exec><Command>"{tray}"</Command><Arguments>{}</Arguments></Exec></Actions>
</Task>
"#,
        opal_ipc::autostart::SIGN_IN
    );
    add_task(TASK, &xml)
}

/// The camera's own webcam gives apps no video while opal-rs runs it, so it is switched off when Windows first sets
/// it up for a camera (the first start of each camera under opal-rs). This task does that as the system.
fn add_camera_task(dir: &Path) -> Result<(), String> {
    let setup = dir.join(SETUP).display().to_string().replace('&', "&amp;");
    let query = "<QueryList><Query Id=\"0\" Path=\"Microsoft-Windows-Kernel-PnP/Configuration\"><Select \
                 Path=\"Microsoft-Windows-Kernel-PnP/Configuration\">*[System[EventID=400]]</Select></Query></QueryList>"
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Switches off the Opal C1's own webcam, which has no video while opal-rs runs the camera, when Windows first sets it up.</Description></RegistrationInfo>
  <Triggers><EventTrigger><Enabled>true</Enabled><Subscription>{query}</Subscription></EventTrigger></Triggers>
  <Principals><Principal id="System"><UserId>S-1-5-18</UserId><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>Queue</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT5M</ExecutionTimeLimit>
  </Settings>
  <Actions Context="System"><Exec><Command>"{setup}"</Command><Arguments>{HIDE_CAMERA}</Arguments></Exec></Actions>
</Task>
"#
    );
    add_task(CAMERA_TASK, &xml)
}

/// Adds or replaces a scheduled task from its XML.
fn add_task(name: &str, xml: &str) -> Result<(), String> {
    // In the install folder, which only administrators can change, not in this account's temporary folder.
    let file = install_dir().join(format!("task-{}.xml", std::process::id()));
    let bytes: Vec<u8> = [0xFEFF]
        .into_iter()
        .chain(xml.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect();
    std::fs::write(&file, bytes).map_err(|e| format!("Cannot write {}: {e}", file.display()))?;
    let mut c = Command::new(windows_program("System32\\schtasks.exe"));
    c.args(["/Create", "/F", "/TN", name, "/XML"]).arg(&file);
    std::os::windows::process::CommandExt::creation_flags(&mut c, opal_ipc::NO_WINDOW);
    let out = c.output();
    let _ = std::fs::remove_file(&file);
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!(
            "Adding the task {name} failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("schtasks: {e}")),
    }
}

fn delete_task(name: &str) {
    let mut c = Command::new(windows_program("System32\\schtasks.exe"));
    c.args(["/Delete", "/F", "/TN", name]);
    std::os::windows::process::CommandExt::creation_flags(&mut c, opal_ipc::NO_WINDOW);
    let _ = c.output();
}

/// Stops this account starting the removed tray at sign-in.
fn stop_autostart() {
    let _ = opal_ipc::autostart::set(false, Path::new(""));
}

/// Offers to delete this account's settings, presets, stickers and camera files.
fn remove_data() {
    let data: Vec<PathBuf> = ["APPDATA", "LOCALAPPDATA"]
        .iter()
        .filter_map(|v| Some(PathBuf::from(std::env::var_os(v)?).join("opal-rs")))
        .filter(|d| d.exists())
        .collect();
    if !data.is_empty()
        && message(
            "Opal C1 is removed.\n\nAlso delete settings, presets, stickers and the camera files copied from \
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
}

/// Asks this account's service to stop and waits for it to exit: it stops the camera first, which takes some
/// seconds after it stops answering. True if it was running.
fn quit_service() -> bool {
    let running_now = opal_ipc::request("quit", Duration::from_secs(3)).is_ok();
    if running_now {
        let start = Instant::now();
        while running("opal-rs.exe") && start.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    running_now
}

/// Whether a program of this name runs, for any account.
fn running(name: &str) -> bool {
    let mut c = Command::new(windows_program("System32\\tasklist.exe"));
    c.args(["/NH", "/FI", &format!("IMAGENAME eq {name}")]);
    std::os::windows::process::CommandExt::creation_flags(&mut c, opal_ipc::NO_WINDOW);
    c.output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains(name))
}

fn stop_running() {
    let _ = quit_service();
    for name in ["opal-rs-tray.exe", "opal-rs-gui.exe", "opal-rs.exe"] {
        let mut c = Command::new(windows_program("System32\\taskkill.exe"));
        c.args(["/F", "/IM", name]);
        std::os::windows::process::CommandExt::creation_flags(&mut c, opal_ipc::NO_WINDOW);
        let _ = c.output();
    }
}

/// Stops Windows' camera service and its monitor, which hold the virtual camera's DLL while loaded.
fn stop_frame_server() {
    for service in ["FrameServerMonitor", "FrameServer"] {
        let mut c = Command::new(windows_program("System32\\sc.exe"));
        c.args(["stop", service]);
        std::os::windows::process::CommandExt::creation_flags(&mut c, opal_ipc::NO_WINDOW);
        let _ = c.output();
    }
}

/// A program in the Windows folder, by full path so a file of the same name next to setup is not run instead.
fn windows_program(name: &str) -> PathBuf {
    let mut buf = [0u16; 260];
    let n = unsafe { GetSystemWindowsDirectoryW(Some(&mut buf)) } as usize;
    let windows = if n > 0 && n < buf.len() {
        PathBuf::from(String::from_utf16_lossy(&buf[..n]))
    } else {
        PathBuf::from("C:\\Windows")
    };
    windows.join(name)
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

const SESSION_MANAGER: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager";
const PENDING: &str = "PendingFileRenameOperations";

/// Takes `dir` and the files in it off the list of deletes Windows does at the next restart.
fn keep_at_restart(dir: &Path) {
    let read = |data: Option<&mut [u16]>| {
        let mut bytes = data.as_ref().map_or(0, |d| (d.len() * 2) as u32);
        let r = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                &HSTRING::from(SESSION_MANAGER),
                &HSTRING::from(PENDING),
                RRF_RT_REG_MULTI_SZ,
                None,
                data.map(|d| d.as_mut_ptr().cast()),
                Some(&mut bytes),
            )
        };
        (r == ERROR_SUCCESS).then_some(bytes as usize / 2)
    };
    let Some(len) = read(None) else { return };
    let mut data = vec![0u16; len];
    let Some(len) = read(Some(&mut data)) else { return };
    data.truncate(len);
    // Strings each end in a zero and the list in one more; an empty string is a delete's target.
    if !data.ends_with(&[0, 0]) {
        return;
    }
    let entries: Vec<String> = data[..data.len() - 2]
        .split(|c| *c == 0)
        .map(String::from_utf16_lossy)
        .collect();
    let Some(kept) = crate::pending::without_deletes_in(&entries, &dir.display().to_string()) else {
        return;
    };
    if kept.is_empty() {
        let _ = unsafe {
            RegDeleteKeyValueW(
                HKEY_LOCAL_MACHINE,
                &HSTRING::from(SESSION_MANAGER),
                &HSTRING::from(PENDING),
            )
        };
        return;
    }
    let mut out: Vec<u16> = kept.iter().flat_map(|s| s.encode_utf16().chain([0])).collect();
    out.push(0);
    let Ok(hkey) = open_key(SESSION_MANAGER) else { return };
    let bytes = unsafe { std::slice::from_raw_parts(out.as_ptr().cast::<u8>(), out.len() * 2) };
    unsafe {
        let _ = RegSetValueExW(hkey, &HSTRING::from(PENDING), None, REG_MULTI_SZ, Some(bytes));
        let _ = RegCloseKey(hkey);
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
