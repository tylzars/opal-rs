//! The camera's own webcam while opal-rs runs it. Windows lists it as "Opal C1", but it gives no video at the sizes
//! apps ask for (its firmware tells Windows every frame is 4K-sized, so smaller frames never arrive), so it is switched
//! off and apps see only "Opal C1 Virtual". The camera's built-in webcam, there when opal-rs is not running, is a
//! separate device and stays on.

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_DEVNODE_STATUS_FLAGS, CM_DISABLE_PERSIST, CM_DRP_CONFIGFLAGS, CM_Disable_DevNode, CM_Enable_DevNode,
    CM_GETIDLIST_FILTER_ENUMERATOR, CM_Get_DevNode_Registry_PropertyW, CM_Get_DevNode_Status,
    CM_Get_Device_ID_List_SizeW, CM_Get_Device_ID_ListW, CM_LOCATE_DEVNODE_PHANTOM, CM_Locate_DevNodeW,
    CM_Open_DevNode_Key, CM_PROB, CM_REGISTRY_HARDWARE, CM_Set_DevNode_Registry_PropertyW, CONFIGFLAG_DISABLED,
    CR_SUCCESS, RegDisposition_OpenAlways,
};
use windows::Win32::System::Registry::{
    HKEY, KEY_ALL_ACCESS, REG_DWORD, RegCloseKey, RegDeleteValueW, RegQueryValueExW, RegSetValueExW,
};
use windows::core::{HSTRING, PCWSTR, w};

/// Windows' device ids for the video function of the firmware opal-rs loads (03e7:f63b, interface 1).
const PREFIX: &str = "USB\\VID_03E7&PID_F63B&MI_01\\";
/// Kept with each device, so it is switched off once and a later choice in Device Manager stands.
const MARK: PCWSTR = w!("OpalRsSwitchedOff");

/// Switches off the camera's own webcam on every camera opal-rs has run here, once each.
pub fn hide() {
    for (dn, key) in devices() {
        if !marked(key) && set_enabled(dn, false) {
            let one = 1u32.to_le_bytes();
            let _ = unsafe { RegSetValueExW(key, MARK, None, REG_DWORD, Some(&one)) };
        }
        let _ = unsafe { RegCloseKey(key) };
    }
}

/// Switches back on what [`hide`] switched off.
pub fn unhide() {
    for (dn, key) in devices() {
        if marked(key) && set_enabled(dn, true) {
            let _ = unsafe { RegDeleteValueW(key, MARK) };
        }
        let _ = unsafe { RegCloseKey(key) };
    }
}

/// The devices, plugged in or not, with their settings key.
fn devices() -> Vec<(u32, HKEY)> {
    ids()
        .iter()
        .filter_map(|id| unsafe {
            let mut dn = 0;
            if CM_Locate_DevNodeW(&mut dn, &HSTRING::from(id.as_str()), CM_LOCATE_DEVNODE_PHANTOM) != CR_SUCCESS {
                return None;
            }
            let mut key = HKEY::default();
            let opened = CM_Open_DevNode_Key(
                dn,
                KEY_ALL_ACCESS.0,
                0,
                RegDisposition_OpenAlways,
                &mut key,
                CM_REGISTRY_HARDWARE,
            );
            (opened == CR_SUCCESS).then_some((dn, key))
        })
        .collect()
}

fn ids() -> Vec<String> {
    let mut len = 0;
    let mut list = Vec::new();
    unsafe {
        if CM_Get_Device_ID_List_SizeW(&mut len, w!("USB"), CM_GETIDLIST_FILTER_ENUMERATOR) != CR_SUCCESS {
            return Vec::new();
        }
        list.resize(len as usize, 0u16);
        if CM_Get_Device_ID_ListW(w!("USB"), &mut list, CM_GETIDLIST_FILTER_ENUMERATOR) != CR_SUCCESS {
            return Vec::new();
        }
    }
    list.split(|&c| c == 0)
        .map(String::from_utf16_lossy)
        .filter(|id| id.to_ascii_uppercase().starts_with(PREFIX))
        .collect()
}

fn marked(key: HKEY) -> bool {
    unsafe { RegQueryValueExW(key, MARK, None, None, None, None) }.is_ok()
}

/// A plugged-in device changes at once; one that is not, or that an app holds, from when it is next plugged in.
fn set_enabled(dn: u32, on: bool) -> bool {
    unsafe {
        let (mut status, mut problem) = (CM_DEVNODE_STATUS_FLAGS(0), CM_PROB(0));
        if CM_Get_DevNode_Status(&mut status, &mut problem, dn, 0) == CR_SUCCESS {
            let done = if on {
                CM_Enable_DevNode(dn, 0)
            } else {
                CM_Disable_DevNode(dn, CM_DISABLE_PERSIST)
            };
            if done == CR_SUCCESS {
                return true;
            }
        }
        let mut flags = 0u32;
        let mut size = 4u32;
        let read = CM_Get_DevNode_Registry_PropertyW(
            dn,
            CM_DRP_CONFIGFLAGS,
            None,
            Some((&mut flags as *mut u32).cast()),
            &mut size,
            0,
        );
        if read != CR_SUCCESS {
            flags = 0;
        }
        if on {
            flags &= !CONFIGFLAG_DISABLED.0;
        } else {
            flags |= CONFIGFLAG_DISABLED.0;
        }
        CM_Set_DevNode_Registry_PropertyW(dn, CM_DRP_CONFIGFLAGS, Some((&flags as *const u32).cast()), 4, 0)
            == CR_SUCCESS
    }
}
