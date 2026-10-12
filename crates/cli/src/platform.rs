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
