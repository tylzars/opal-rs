//! Start at sign-in is per account (its Run value), so uninstalling clears it in every account on the PC, not only
//! the one running setup: signed-in accounts through their open registry, the others by opening theirs for a moment.

use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES,
    TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegDeleteKeyValueW, RegEnumKeyExW,
    RegGetValueW, RegLoadKeyW, RegOpenKeyExW, RegUnLoadKeyW,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const PROFILES: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList";

/// Removes the `name` Run value, and Windows' switch for it, from every account.
pub fn clear_run_value(name: &str) {
    let loaded = subkeys(HKEY_USERS, "");
    for sid in loaded.iter().filter(|s| is_person(s)) {
        clear(sid, name);
    }
    let others: Vec<String> = subkeys(HKEY_LOCAL_MACHINE, PROFILES)
        .into_iter()
        .filter(|s| is_person(s) && !loaded.iter().any(|l| l.eq_ignore_ascii_case(s)))
        .collect();
    if others.is_empty() || !privileges() {
        return;
    }
    for sid in others {
        let Some(home) = string(HKEY_LOCAL_MACHINE, &format!(r"{PROFILES}\{sid}"), "ProfileImagePath") else {
            continue;
        };
        let hive = std::path::Path::new(&home).join("NTUSER.DAT");
        // Its own name, so one that cannot be closed again does not stop the next.
        let mount = format!("opal-rs-{sid}");
        unsafe {
            if RegLoadKeyW(
                HKEY_USERS,
                &HSTRING::from(mount.as_str()),
                &HSTRING::from(hive.as_os_str()),
            )
            .is_ok()
            {
                clear(&mount, name);
                // Left open, it would stand in the way of that account signing in until a restart; something else
                // reading it can hold it for a moment.
                for _ in 0..10 {
                    if RegUnLoadKeyW(HKEY_USERS, &HSTRING::from(mount.as_str())).is_ok() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        }
    }
}

/// A person's account, local or work/school (Entra), not the system's own (S-1-5-18 and so on) or a `_Classes` key.
fn is_person(sid: &str) -> bool {
    (sid.starts_with("S-1-5-21-") || sid.starts_with("S-1-12-1-")) && !sid.ends_with("_Classes")
}

fn clear(root: &str, name: &str) {
    for key in [RUN, APPROVED] {
        unsafe {
            let _ = RegDeleteKeyValueW(
                HKEY_USERS,
                &HSTRING::from(format!(r"{root}\{key}")),
                &HSTRING::from(name),
            );
        }
    }
}

fn subkeys(root: HKEY, path: &str) -> Vec<String> {
    let mut names = Vec::new();
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(root, &HSTRING::from(path), None, KEY_READ, &mut key).is_err() {
            return names;
        }
        for i in 0.. {
            let mut buf = [0u16; 256];
            let mut len = buf.len() as u32;
            if RegEnumKeyExW(key, i, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None).is_err() {
                break;
            }
            names.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
        let _ = RegCloseKey(key);
    }
    names
}

/// A string value; `%SystemDrive%` and the like come back expanded.
fn string(root: HKEY, path: &str, value: &str) -> Option<String> {
    let mut buf = [0u16; 520];
    let mut size = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            root,
            &HSTRING::from(path),
            &HSTRING::from(value),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
        .ok()
        .ok()?;
    }
    Some(String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]))
}

/// Opening another account's registry needs the backup and restore privileges, which administrators hold but have
/// switched off.
fn privileges() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut ok = true;
        for name in [w!("SeBackupPrivilege"), w!("SeRestorePrivilege")] {
            let mut luid = LUID::default();
            ok &= LookupPrivilegeValueW(PCWSTR::null(), name, &mut luid).is_ok()
                && AdjustTokenPrivileges(
                    token,
                    false,
                    Some(&TOKEN_PRIVILEGES {
                        PrivilegeCount: 1,
                        Privileges: [LUID_AND_ATTRIBUTES {
                            Luid: luid,
                            Attributes: SE_PRIVILEGE_ENABLED,
                        }],
                    }),
                    0,
                    None,
                    None,
                )
                .is_ok();
        }
        let _ = CloseHandle(token);
        ok
    }
}
