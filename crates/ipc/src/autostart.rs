//! Starting at sign-in, for this account: the tray icon starts then, and it starts the service. Uses the system's
//! own switch, so its startup settings agree: the `opal-rs` value under the account's Run key on Windows, the
//! account's copy of the tray's autostart file on Linux.

use std::io;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

/// Passed to the tray by the Windows sign-in task, which starts it for every account: it then runs only if this
/// account starts it at sign-in.
pub const SIGN_IN: &str = "--sign-in";

/// The package's autostart file, on for every account unless an account's own copy hides it.
#[cfg(unix)]
const SYSTEM_FILE: &str = "/etc/xdg/autostart/opal-rs-tray.desktop";

#[cfg(unix)]
fn user_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("autostart").join("opal-rs-tray.desktop"))
}

/// An autostart file the desktop skips: hidden, or switched off by GNOME's startup settings.
#[cfg(unix)]
fn skipped(text: &str) -> bool {
    text.lines()
        .map(|l| l.split_whitespace().collect::<String>())
        .any(|l| l == "Hidden=true" || l == "X-GNOME-Autostart-enabled=false")
}

/// True when the tray icon starts at sign-in.
#[cfg(unix)]
pub fn enabled() -> bool {
    match user_file().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(text) => !skipped(&text),
        None => Path::new(SYSTEM_FILE).exists(),
    }
}

/// Starts the tray icon at sign-in or not. `tray` is the program to start (only used on Windows).
#[cfg(unix)]
pub fn set(on: bool, _tray: &Path) -> io::Result<()> {
    let path = user_file().ok_or_else(|| io::Error::other("no home folder"))?;
    if on && Path::new(SYSTEM_FILE).exists() {
        // The package's file applies again.
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let hidden = if on { "" } else { "Hidden=true\n" };
    std::fs::write(
        path,
        format!(
            "[Desktop Entry]\nType=Application\nName=Opal C1 tray icon\nExec=opal-rs-tray\nTryExec=opal-rs-tray\n\
             NoDisplay=true\n{hidden}"
        ),
    )
}

#[cfg(windows)]
mod registry {
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    /// Where Windows' own startup settings mark a Run entry switched off.
    const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    const NAME: &str = "opal-rs";
    const NOT_FOUND: u32 = 2;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    fn check(code: u32) -> std::io::Result<()> {
        match code {
            0 | NOT_FOUND => Ok(()),
            e => Err(std::io::Error::from_raw_os_error(e as i32)),
        }
    }

    pub fn enabled() -> bool {
        let name = wide(NAME);
        let mut size = 0;
        let present = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                wide(RUN).as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        } == 0;
        // An odd first byte means switched off.
        let mut flags = [0u8; 64];
        let mut size = flags.len() as u32;
        let off = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                wide(APPROVED).as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_BINARY,
                std::ptr::null_mut(),
                flags.as_mut_ptr().cast(),
                &mut size,
            )
        } == 0
            && size > 0
            && flags[0] & 1 == 1;
        present && !off
    }

    pub fn set(on: bool, tray: &std::path::Path) -> std::io::Result<()> {
        let name = wide(NAME);
        unsafe {
            // Switched on here also clears Windows' own switch, so both agree.
            RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(APPROVED).as_ptr(), name.as_ptr());
            if !on {
                return check(RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(RUN).as_ptr(), name.as_ptr()));
            }
            let value = wide(&format!("\"{}\"", tray.display()));
            check(RegSetKeyValueW(
                HKEY_CURRENT_USER,
                wide(RUN).as_ptr(),
                name.as_ptr(),
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            ))
        }
    }
}

/// True when the tray icon starts at sign-in.
#[cfg(windows)]
pub fn enabled() -> bool {
    registry::enabled()
}

/// Starts the tray icon at sign-in or not. `tray` is the program to start (only used on Windows).
#[cfg(windows)]
pub fn set(on: bool, tray: &Path) -> io::Result<()> {
    if on && !tray.is_absolute() {
        return Err(io::Error::other(format!("{} not found", tray.display())));
    }
    registry::set(on, tray)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn reads_hidden_files() {
        use super::skipped;
        assert!(skipped("[Desktop Entry]\nExec=opal-rs-tray\nHidden=true\n"));
        assert!(skipped("[Desktop Entry]\nX-GNOME-Autostart-enabled = false\n"));
        assert!(!skipped(
            "[Desktop Entry]\nExec=opal-rs-tray\nX-GNOME-Autostart-enabled=true\n"
        ));
    }
}
