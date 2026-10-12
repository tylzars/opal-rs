//! What differs between Linux and Windows apart from the camera link: the settings folder, the local time and
//! whether the screen is locked.

use std::path::PathBuf;

/// Where settings, presets, stickers and pictures are kept: `~/.config/opal-rs` on Linux, `%APPDATA%\opal-rs` on
/// Windows.
pub fn config_dir() -> PathBuf {
    #[cfg(unix)]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    base.unwrap_or_else(std::env::temp_dir).join("opal-rs")
}

/// Where installed copies keep the camera files: `~/.local/share/opal-rs` on Linux, `%LOCALAPPDATA%\opal-rs` on
/// Windows.
pub fn data_dir() -> PathBuf {
    #[cfg(unix)]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    base.unwrap_or_else(std::env::temp_dir).join("opal-rs")
}

/// Local wall-clock time as HH:MM.
pub fn local_time() -> String {
    #[cfg(unix)]
    let (h, m) = {
        let now = unsafe { libc::time(std::ptr::null_mut()) };
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&now, &mut tm) };
        (tm.tm_hour as u16, tm.tm_min as u16)
    };
    #[cfg(windows)]
    let (h, m) = {
        let mut t = unsafe { std::mem::zeroed() };
        unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
        (t.wHour, t.wMinute)
    };
    format!("{h:02}:{m:02}")
}

/// Tells whether the user's desktop is locked.
pub struct ScreenLock {
    /// The logind session of the user's desktop.
    #[cfg(unix)]
    session: Option<String>,
}

impl ScreenLock {
    pub fn new() -> Self {
        #[cfg(unix)]
        {
            let session = (|| {
                let out = std::process::Command::new("loginctl")
                    .args(["show-user", &std::env::var("USER").ok()?, "-p", "Display", "--value"])
                    .output()
                    .ok()?;
                let id = String::from_utf8(out.stdout).ok()?.trim().to_owned();
                (!id.is_empty()).then_some(id)
            })();
            Self { session }
        }
        #[cfg(windows)]
        Self {}
    }

    /// Whether the screen is locked, or `None` when that cannot be told.
    pub fn locked(&self) -> Option<bool> {
        #[cfg(unix)]
        {
            // As the lock screen reports it to logind.
            let out = std::process::Command::new("loginctl")
                .args(["show-session", self.session.as_deref()?, "-p", "LockedHint", "--value"])
                .output()
                .ok()?;
            match String::from_utf8_lossy(&out.stdout).trim() {
                "yes" => Some(true),
                "no" => Some(false),
                _ => None,
            }
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::RemoteDesktop::{
                WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTS_SESSIONSTATE_LOCK, WTSFreeMemory, WTSINFOEXW,
                WTSQuerySessionInformationW, WTSSessionInfoEx,
            };
            let mut info: *mut u16 = std::ptr::null_mut();
            let mut len = 0u32;
            let ok = unsafe {
                WTSQuerySessionInformationW(
                    WTS_CURRENT_SERVER_HANDLE,
                    WTS_CURRENT_SESSION,
                    WTSSessionInfoEx,
                    &mut info,
                    &mut len,
                )
            };
            if ok == 0 || info.is_null() {
                return None;
            }
            let ex = unsafe { &*(info as *const WTSINFOEXW) };
            let flags = unsafe { ex.Data.WTSInfoExLevel1.SessionFlags } as u32;
            let locked = (ex.Level == 1).then_some(flags == WTS_SESSIONSTATE_LOCK);
            unsafe { WTSFreeMemory(info.cast()) };
            locked
        }
    }
}

/// Apps using a camera right now, from Windows' record of camera use (started, not yet stopped).
#[cfg(windows)]
pub fn camera_apps() -> Vec<String> {
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_QWORD, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
    };
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let open = |parent: HKEY, path: &str| {
        let mut key: HKEY = std::ptr::null_mut();
        (unsafe { RegOpenKeyExW(parent, wide(path).as_ptr(), 0, KEY_READ, &mut key) } == 0).then_some(key)
    };
    let subkeys = |key: HKEY| {
        let mut names = Vec::new();
        for i in 0.. {
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let r = unsafe {
                RegEnumKeyExW(
                    key,
                    i,
                    buf.as_mut_ptr(),
                    &mut len,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if r != 0 {
                break;
            }
            names.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
        names
    };
    let qword = |key: HKEY, sub: &str, name: &str| {
        let mut v = 0u64;
        let mut size = 8u32;
        let r = unsafe {
            RegGetValueW(
                key,
                wide(sub).as_ptr(),
                wide(name).as_ptr(),
                RRF_RT_REG_QWORD,
                std::ptr::null_mut(),
                (&mut v as *mut u64).cast(),
                &mut size,
            )
        };
        (r == 0).then_some(v)
    };
    let in_use = |key: HKEY, sub: &str| {
        qword(key, sub, "LastUsedTimeStop") == Some(0) && qword(key, sub, "LastUsedTimeStart").is_some_and(|t| t != 0)
    };
    let mut apps = Vec::new();
    let base = "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\webcam";
    let Some(webcam) = open(HKEY_CURRENT_USER, base) else {
        return apps;
    };
    for name in subkeys(webcam) {
        if name == "NonPackaged" {
            if let Some(np) = open(webcam, "NonPackaged") {
                // Desktop programs, keyed by their path with '#' for each backslash.
                for path in subkeys(np) {
                    if in_use(np, &path) {
                        let file = path.rsplit('#').next().unwrap_or(&path);
                        apps.push(file.trim_end_matches(".exe").trim_end_matches(".EXE").to_owned());
                    }
                }
                unsafe { RegCloseKey(np) };
            }
        } else if in_use(webcam, &name) {
            // Store apps, keyed by package family name.
            apps.push(name.split('_').next().unwrap_or(&name).to_owned());
        }
    }
    unsafe { RegCloseKey(webcam) };
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_looks_like_a_clock() {
        let t = local_time();
        assert_eq!(t.len(), 5);
        assert_eq!(&t[2..3], ":");
    }

    #[test]
    fn config_dir_is_ours() {
        assert!(config_dir().ends_with("opal-rs"));
    }
}
