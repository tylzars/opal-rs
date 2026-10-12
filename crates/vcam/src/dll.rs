//! The DLL's COM entry points: the class factory for the media source's activator, and registration.

use std::ffi::c_void;

use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, E_POINTER, HMODULE, S_FALSE, S_OK};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW,
    RegSetValueExW,
};
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::core::{BOOL, GUID, HRESULT, HSTRING, IUnknown, PCWSTR, PWSTR, Ref, Result, implement};
use windows_core::Interface;

use crate::source::Source;

static mut MODULE: HMODULE = HMODULE(std::ptr::null_mut());

fn clsid() -> GUID {
    GUID::from_u128(crate::CLSID)
}

#[unsafe(no_mangle)]
extern "system" fn DllMain(module: HMODULE, reason: u32, _: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        unsafe { MODULE = module };
    }
    true.into()
}

#[unsafe(no_mangle)]
extern "system" fn DllGetClassObject(rclsid: *const GUID, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = std::ptr::null_mut() };
    if unsafe { *rclsid } != clsid() {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: IClassFactory = Factory.into();
    unsafe { factory.query(riid, ppv) }
}

#[unsafe(no_mangle)]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    // Stays loaded; Frame Server unloads it with its process.
    S_FALSE
}

/// Registers the media source for the machine. Run as administrator: `regsvr32 opal_vcam.dll`.
#[unsafe(no_mangle)]
extern "system" fn DllRegisterServer() -> HRESULT {
    match register() {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllUnregisterServer() -> HRESULT {
    let key = HSTRING::from(class_key());
    match unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &key) }.ok() {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}

fn class_key() -> String {
    format!("Software\\Classes\\CLSID\\{{{:?}}}", clsid())
}

fn register() -> Result<()> {
    let mut path = [0u16; 1024];
    let len = unsafe { GetModuleFileNameW(Some(MODULE), &mut path) } as usize;
    if len == 0 {
        return Err(windows::core::Error::from_thread());
    }
    let path = String::from_utf16_lossy(&path[..len]);
    set_value(&class_key(), None, crate::NAME)?;
    let inproc = format!("{}\\InprocServer32", class_key());
    set_value(&inproc, None, &path)?;
    set_value(&inproc, Some("ThreadingModel"), "Both")
}

fn set_value(key: &str, name: Option<&str>, value: &str) -> Result<()> {
    let mut hkey = HKEY::default();
    unsafe {
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
        .ok()?;
        let data: Vec<u16> = value.encode_utf16().chain([0]).collect();
        let bytes = std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2);
        let name = name.map(HSTRING::from);
        let r = RegSetValueExW(
            hkey,
            name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
            None,
            REG_SZ,
            Some(bytes),
        )
        .ok();
        let _ = RegCloseKey(hkey);
        r
    }
}

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(&self, outer: Ref<IUnknown>, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        if outer.is_some() {
            return Err(windows::Win32::Foundation::CLASS_E_NOAGGREGATION.into());
        }
        let activate: IMFActivate = Activate::new()?.into();
        unsafe { activate.query(riid, ppv).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> Result<()> {
        Ok(())
    }
}

/// What Frame Server creates first: it hands out the media source when activated. Its attributes are a plain
/// attribute store that Frame Server may fill in.
#[implement(IMFActivate)]
struct Activate {
    attributes: IMFAttributes,
    /// The source handed out, shut down through the activate when Frame Server is done with it.
    source: std::sync::Mutex<Option<IMFMediaSourceEx>>,
}

impl Activate {
    fn new() -> Result<Self> {
        let mut attributes = None;
        unsafe { MFCreateAttributes(&mut attributes, 1)? };
        Ok(Self {
            attributes: attributes.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?,
            source: std::sync::Mutex::new(None),
        })
    }
}

impl IMFActivate_Impl for Activate_Impl {
    fn ActivateObject(&self, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        let mut slot = self.source.lock().unwrap();
        let source = match &*slot {
            Some(s) => s.clone(),
            None => {
                let s: IMFMediaSourceEx = Source::new()?.into();
                *slot = Some(s.clone());
                s
            }
        };
        unsafe { source.query(riid, ppv).ok() }
    }

    fn ShutdownObject(&self) -> Result<()> {
        if let Some(s) = self.source.lock().unwrap().take() {
            unsafe { s.Shutdown()? };
        }
        Ok(())
    }

    fn DetachObject(&self) -> Result<()> {
        self.source.lock().unwrap().take();
        Ok(())
    }
}

/// Every attribute call goes to the store.
impl IMFAttributes_Impl for Activate_Impl {
    fn GetItem(&self, key: *const GUID, value: *mut PROPVARIANT) -> Result<()> {
        unsafe { self.attributes.GetItem(key, Some(value)) }
    }
    fn GetItemType(&self, key: *const GUID) -> Result<MF_ATTRIBUTE_TYPE> {
        unsafe { self.attributes.GetItemType(key) }
    }
    fn CompareItem(&self, key: *const GUID, value: *const PROPVARIANT) -> Result<BOOL> {
        unsafe { self.attributes.CompareItem(key, value) }
    }
    fn Compare(&self, theirs: Ref<IMFAttributes>, match_type: MF_ATTRIBUTES_MATCH_TYPE) -> Result<BOOL> {
        unsafe { self.attributes.Compare(theirs.as_ref(), match_type) }
    }
    fn GetUINT32(&self, key: *const GUID) -> Result<u32> {
        unsafe { self.attributes.GetUINT32(key) }
    }
    fn GetUINT64(&self, key: *const GUID) -> Result<u64> {
        unsafe { self.attributes.GetUINT64(key) }
    }
    fn GetDouble(&self, key: *const GUID) -> Result<f64> {
        unsafe { self.attributes.GetDouble(key) }
    }
    fn GetGUID(&self, key: *const GUID) -> Result<GUID> {
        unsafe { self.attributes.GetGUID(key) }
    }
    fn GetStringLength(&self, key: *const GUID) -> Result<u32> {
        unsafe { self.attributes.GetStringLength(key) }
    }
    fn GetString(&self, key: *const GUID, value: PWSTR, size: u32, len: *mut u32) -> Result<()> {
        unsafe {
            let buf = std::slice::from_raw_parts_mut(value.0, size as usize);
            self.attributes.GetString(key, buf, Some(len))
        }
    }
    fn GetAllocatedString(&self, key: *const GUID, value: *mut PWSTR, len: *mut u32) -> Result<()> {
        unsafe { self.attributes.GetAllocatedString(key, value, len) }
    }
    fn GetBlobSize(&self, key: *const GUID) -> Result<u32> {
        unsafe { self.attributes.GetBlobSize(key) }
    }
    fn GetBlob(&self, key: *const GUID, buf: *mut u8, size: u32, len: *mut u32) -> Result<()> {
        unsafe {
            let buf = std::slice::from_raw_parts_mut(buf, size as usize);
            self.attributes.GetBlob(key, buf, Some(len))
        }
    }
    fn GetAllocatedBlob(&self, key: *const GUID, buf: *mut *mut u8, size: *mut u32) -> Result<()> {
        unsafe { self.attributes.GetAllocatedBlob(key, buf, size) }
    }
    fn GetUnknown(&self, key: *const GUID, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        // The typed wrapper fixes the interface; forward the caller's id as is.
        unsafe {
            (Interface::vtable(&self.attributes).GetUnknown)(Interface::as_raw(&self.attributes), key, riid, ppv).ok()
        }
    }
    fn SetItem(&self, key: *const GUID, value: *const PROPVARIANT) -> Result<()> {
        unsafe { self.attributes.SetItem(key, value) }
    }
    fn DeleteItem(&self, key: *const GUID) -> Result<()> {
        unsafe { self.attributes.DeleteItem(key) }
    }
    fn DeleteAllItems(&self) -> Result<()> {
        unsafe { self.attributes.DeleteAllItems() }
    }
    fn SetUINT32(&self, key: *const GUID, value: u32) -> Result<()> {
        unsafe { self.attributes.SetUINT32(key, value) }
    }
    fn SetUINT64(&self, key: *const GUID, value: u64) -> Result<()> {
        unsafe { self.attributes.SetUINT64(key, value) }
    }
    fn SetDouble(&self, key: *const GUID, value: f64) -> Result<()> {
        unsafe { self.attributes.SetDouble(key, value) }
    }
    fn SetGUID(&self, key: *const GUID, value: *const GUID) -> Result<()> {
        unsafe { self.attributes.SetGUID(key, value) }
    }
    fn SetString(&self, key: *const GUID, value: &PCWSTR) -> Result<()> {
        unsafe { self.attributes.SetString(key, *value) }
    }
    fn SetBlob(&self, key: *const GUID, buf: *const u8, size: u32) -> Result<()> {
        unsafe {
            self.attributes
                .SetBlob(key, std::slice::from_raw_parts(buf, size as usize))
        }
    }
    fn SetUnknown(&self, key: *const GUID, value: Ref<IUnknown>) -> Result<()> {
        unsafe { self.attributes.SetUnknown(key, value.as_ref()) }
    }
    fn LockStore(&self) -> Result<()> {
        unsafe { self.attributes.LockStore() }
    }
    fn UnlockStore(&self) -> Result<()> {
        unsafe { self.attributes.UnlockStore() }
    }
    fn GetCount(&self) -> Result<u32> {
        unsafe { self.attributes.GetCount() }
    }
    fn GetItemByIndex(&self, index: u32, key: *mut GUID, value: *mut PROPVARIANT) -> Result<()> {
        unsafe { self.attributes.GetItemByIndex(index, key, Some(value)) }
    }
    fn CopyAllItems(&self, dest: Ref<IMFAttributes>) -> Result<()> {
        unsafe { self.attributes.CopyAllItems(dest.as_ref()) }
    }
}
