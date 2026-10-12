//! The service's side: puts "Opal C1 Virtual" on the system for as long as the service runs, and writes frames to
//! the source's shared buffer while an app has the camera open.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::MediaFoundation::{
    IMFVirtualCamera, MF_VERSION, MFSTARTUP_FULL, MFStartup, MFVirtualCameraAccess, MFVirtualCameraAccess_CurrentUser,
    MFVirtualCameraLifetime, MFVirtualCameraLifetime_Session, MFVirtualCameraType,
    MFVirtualCameraType_SoftwareCameraSource,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Memory::{
    FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile,
};
use windows::core::{GUID, HRESULT, HSTRING, Interface, PCWSTR, s, w};

use crate::shared::{self, View};

/// `MFCreateVirtualCamera`, looked up when needed: Windows 10 does not have it, and linking it would keep the
/// program from starting there at all.
type CreateVirtualCamera = unsafe extern "system" fn(
    MFVirtualCameraType,
    MFVirtualCameraLifetime,
    MFVirtualCameraAccess,
    PCWSTR,
    PCWSTR,
    *const GUID,
    u32,
    *mut *mut std::ffi::c_void,
) -> HRESULT;

fn create_function() -> Option<CreateVirtualCamera> {
    unsafe {
        let lib = LoadLibraryW(w!("mfsensorgroup.dll")).ok()?;
        let f = GetProcAddress(lib, s!("MFCreateVirtualCamera"))?;
        Some(std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            CreateVirtualCamera,
        >(f))
    }
}

/// True when this Windows can show a virtual camera (Windows 11 and newer).
pub fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| create_function().is_some())
}

/// The camera as registered with Windows for this session. Dropping it takes the camera away again.
pub struct VirtualCamera {
    camera: IMFVirtualCamera,
}

impl VirtualCamera {
    /// Fails when the media source is not registered (`regsvr32 opal_vcam.dll`, once, as administrator) or Windows
    /// is older than Windows 11.
    pub fn create() -> Result<Self, String> {
        unsafe {
            // Already initialised on this thread is fine.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| format!("Media Foundation: {e}"))?;
            let create = create_function().ok_or("Windows 11 or newer is needed for the virtual camera")?;
            let id = HSTRING::from(format!("{{{:?}}}", GUID::from_u128(crate::CLSID)));
            let name = HSTRING::from(crate::NAME);
            let mut raw = std::ptr::null_mut();
            create(
                MFVirtualCameraType_SoftwareCameraSource,
                MFVirtualCameraLifetime_Session,
                MFVirtualCameraAccess_CurrentUser,
                PCWSTR(name.as_ptr()),
                PCWSTR(id.as_ptr()),
                std::ptr::null(),
                0,
                &mut raw,
            )
            .ok()
            .map_err(|e| format!("cannot create the virtual camera ({e}); is opal_vcam.dll registered?"))?;
            let camera = IMFVirtualCamera::from_raw(raw);
            camera
                .Start(None)
                .map_err(|e| format!("cannot start the virtual camera: {e}"))?;
            Ok(Self { camera })
        }
    }
}

impl Drop for VirtualCamera {
    fn drop(&mut self) {
        let _ = unsafe { self.camera.Shutdown() };
    }
}

/// The shared frame buffer, opened once the source has made it (when an app first opens the camera).
pub struct Frames {
    mapping: Option<(HANDLE, MEMORY_MAPPED_VIEW_ADDRESS, View)>,
    last_try: Option<Instant>,
}

// The view is atomics plus the sequence-guarded frame; the handle is only closed on drop.
unsafe impl Send for Frames {}

impl Default for Frames {
    fn default() -> Self {
        Self::new()
    }
}

impl Frames {
    pub fn new() -> Self {
        Self {
            mapping: None,
            last_try: None,
        }
    }

    fn view(&mut self) -> Option<&View> {
        if self.mapping.is_none() && self.last_try.is_none_or(|t| t.elapsed() > Duration::from_secs(1)) {
            self.last_try = Some(Instant::now());
            self.mapping = open();
        }
        self.mapping.as_ref().map(|m| &m.2).filter(|v| v.valid())
    }

    /// Whether an app is taking frames from the camera.
    pub fn streaming(&mut self) -> bool {
        self.view().is_some_and(View::streaming)
    }

    /// Hands one NV12 frame to the camera.
    pub fn write(&mut self, frame: &[u8], width: usize, height: usize) {
        if let Some(v) = self.view() {
            v.write(frame, width, height);
        }
    }
}

fn open() -> Option<(HANDLE, MEMORY_MAPPED_VIEW_ADDRESS, View)> {
    let access = FILE_MAP_READ.0 | FILE_MAP_WRITE.0;
    let handle = unsafe { OpenFileMappingW(access, false, &HSTRING::from(shared::NAME)).ok()? };
    let address = unsafe { MapViewOfFile(handle, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, shared::SIZE) };
    if address.Value.is_null() {
        let _ = unsafe { CloseHandle(handle) };
        return None;
    }
    Some((handle, address, unsafe { View::new(address.Value as *mut u8) }))
}

impl Drop for Frames {
    fn drop(&mut self) {
        if let Some((handle, address, _)) = self.mapping.take() {
            unsafe {
                let _ = UnmapViewOfFile(address);
                let _ = CloseHandle(handle);
            }
        }
    }
}
