//! The media source Frame Server loads for "Opal C1 Virtual": one live NV12 video stream, fed from the shared frame
//! buffer the service writes to.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    CloseHandle, E_FAIL, ERROR_SET_NOT_FOUND, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree,
};
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsControl_Impl, KSIDENTIFIER, PINNAME_VIDEO_CAPTURE};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READWRITE, UnmapViewOfFile,
};
use windows::core::{GUID, HRESULT, HSTRING, IUnknown, Ref, Result, implement};
use windows_core::{ComObject, IUnknownImpl, Interface};

use crate::shared::{self, View};

/// Sizes offered to apps, the first being the default. Frames of another size are resized to the one picked.
const SIZES: [(u32, u32); 4] = [(1920, 1080), (1280, 720), (2560, 1440), (3840, 2160)];
const FPS: u32 = 30;
const FRAME_TIME: Duration = Duration::from_nanos(1_000_000_000 / FPS as u64);
/// Media Foundation time is in 100 ns units.
const FRAME_UNITS: i64 = 10_000_000 / FPS as i64;

fn media_type(width: u32, height: u32) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | height as u64)?;
        t.SetUINT64(&MF_MT_FRAME_RATE, ((FPS as u64) << 32) | 1)?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        t.SetUINT32(&MF_MT_DEFAULT_STRIDE, width)?;
        t.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
        t.SetUINT32(&MF_MT_SAMPLE_SIZE, width * height * 3 / 2)?;
        Ok(t)
    }
}

/// The shared frame buffer, created by the source.
pub struct Shared {
    handle: HANDLE,
    address: MEMORY_MAPPED_VIEW_ADDRESS,
    pub view: View,
}

unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

impl Shared {
    /// Creates the buffer, open to the interactive user so the service in the user's session can write to it.
    fn create() -> Result<Self> {
        // System, local service and administrators: everything; the signed-in user: read and write.
        let sddl = HSTRING::from("D:P(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;BA)(A;;GRGW;;;IU)");
        let mut sd = PSECURITY_DESCRIPTOR::default();
        unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, SDDL_REVISION_1, &mut sd, None)? };
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let size = shared::SIZE as u64;
        let create = |name: &str| unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                Some(&sa),
                PAGE_READWRITE,
                (size >> 32) as u32,
                size as u32,
                &HSTRING::from(name),
            )
        };
        let handle = create(shared::NAME);
        unsafe { LocalFree(Some(HLOCAL(sd.0))) };
        let handle = handle?;
        let address = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, shared::SIZE) };
        if address.Value.is_null() {
            let e = windows::core::Error::from_thread();
            unsafe { CloseHandle(handle)? };
            return Err(e);
        }
        let view = unsafe { View::new(address.Value as *mut u8) };
        view.init();
        Ok(Self { handle, address, view })
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.view.set_streaming(false);
        unsafe {
            let _ = UnmapViewOfFile(self.address);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// No camera properties are offered.
fn no_property() -> Result<()> {
    Err(HRESULT::from_win32(ERROR_SET_NOT_FOUND.0).into())
}

#[implement(IMFMediaSourceEx, IMFGetService, IKsControl)]
pub struct Source {
    queue: IMFMediaEventQueue,
    attributes: IMFAttributes,
    descriptor: IMFPresentationDescriptor,
    stream_descriptor: IMFStreamDescriptor,
    shared: Option<Arc<Shared>>,
    state: Mutex<SourceState>,
}

#[derive(Default)]
struct SourceState {
    stream: Option<ComObject<Stream>>,
    shutdown: bool,
}

impl Source {
    pub fn new() -> Result<Self> {
        unsafe {
            let types: Vec<Option<IMFMediaType>> = SIZES
                .iter()
                .map(|&(w, h)| media_type(w, h).map(Some))
                .collect::<Result<_>>()?;
            let sd = MFCreateStreamDescriptor(0, &types)?;
            sd.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
            sd.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, 0)?;
            sd.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
            sd.SetUINT32(
                &MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES,
                MFFrameSourceTypes_Color.0 as u32,
            )?;
            sd.GetMediaTypeHandler()?.SetCurrentMediaType(types[0].as_ref())?;
            let descriptor = MFCreatePresentationDescriptor(Some(&[Some(sd.clone())]))?;
            descriptor.SelectStream(0)?;
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 1)?;
            let attributes = attributes.ok_or_else(|| windows::core::Error::from(E_FAIL))?;
            attributes.SetString(&MFT_FRIENDLY_NAME_Attribute, &HSTRING::from(crate::NAME))?;
            Ok(Self {
                queue: MFCreateEventQueue()?,
                attributes,
                descriptor,
                stream_descriptor: sd,
                // Without the buffer the camera still opens, showing black.
                shared: Shared::create().ok().map(Arc::new),
                state: Mutex::new(SourceState::default()),
            })
        }
    }
}

impl IMFMediaEventGenerator_Impl for Source_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        unsafe { self.queue.GetEvent(flags.0) }
    }
    fn BeginGetEvent(&self, callback: Ref<IMFAsyncCallback>, state: Ref<IUnknown>) -> Result<()> {
        unsafe { self.queue.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }
    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> Result<IMFMediaEvent> {
        unsafe { self.queue.EndGetEvent(result.as_ref()) }
    }
    fn QueueEvent(&self, met: u32, ext: *const GUID, status: HRESULT, value: *const PROPVARIANT) -> Result<()> {
        unsafe { self.queue.QueueEventParamVar(met, ext, status, value) }
    }
}

impl IMFMediaSource_Impl for Source_Impl {
    fn GetCharacteristics(&self) -> Result<u32> {
        self.check()?;
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> Result<IMFPresentationDescriptor> {
        self.check()?;
        unsafe { self.descriptor.Clone() }
    }

    fn Start(
        &self,
        descriptor: Ref<IMFPresentationDescriptor>,
        _format: *const GUID,
        start: *const PROPVARIANT,
    ) -> Result<()> {
        self.check()?;
        // The app's choice of size lives in the stream descriptor's handler, shared with ours.
        let _ = descriptor;
        let mut st = self.state.lock().unwrap();
        let new = st.stream.is_none();
        if new {
            let ex: IMFMediaSourceEx = self.to_interface();
            let source: IMFMediaSource = ex.cast()?;
            st.stream = Some(ComObject::new(Stream::new(
                source,
                self.stream_descriptor.clone(),
                self.shared.clone(),
            )?));
        }
        let stream = st.stream.as_ref().unwrap();
        let stream2: IMFMediaStream2 = stream.to_interface();
        let unknown: IMFMediaStream = stream2.cast()?;
        unsafe {
            let event = if new { MENewStream } else { MEUpdatedStream };
            self.queue
                .QueueEventParamUnk(event.0 as u32, &GUID::zeroed(), HRESULT(0), &unknown)?;
        }
        stream.set_state(MF_STREAM_STATE_RUNNING)?;
        unsafe {
            self.queue
                .QueueEventParamVar(MESourceStarted.0 as u32, &GUID::zeroed(), HRESULT(0), start)?
        };
        if let Some(sh) = &self.shared {
            sh.view.set_streaming(true);
        }
        Ok(())
    }

    fn Stop(&self) -> Result<()> {
        self.check()?;
        let st = self.state.lock().unwrap();
        if let Some(s) = &st.stream {
            s.set_state(MF_STREAM_STATE_STOPPED)?;
        }
        if let Some(sh) = &self.shared {
            sh.view.set_streaming(false);
        }
        unsafe {
            self.queue
                .QueueEventParamVar(MESourceStopped.0 as u32, &GUID::zeroed(), HRESULT(0), std::ptr::null())
        }
    }

    fn Pause(&self) -> Result<()> {
        Err(MF_E_INVALID_STATE_TRANSITION.into())
    }

    fn Shutdown(&self) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        st.shutdown = true;
        if let Some(s) = st.stream.take() {
            s.shutdown();
        }
        if let Some(sh) = &self.shared {
            sh.view.set_streaming(false);
        }
        unsafe { self.queue.Shutdown() }
    }
}

impl Source_Impl {
    fn check(&self) -> Result<()> {
        if self.state.lock().unwrap().shutdown {
            Err(MF_E_SHUTDOWN.into())
        } else {
            Ok(())
        }
    }
}

impl IMFMediaSourceEx_Impl for Source_Impl {
    fn GetSourceAttributes(&self) -> Result<IMFAttributes> {
        self.check()?;
        Ok(self.attributes.clone())
    }

    fn GetStreamAttributes(&self, id: u32) -> Result<IMFAttributes> {
        self.check()?;
        if id != 0 {
            return Err(MF_E_INVALIDSTREAMNUMBER.into());
        }
        self.stream_descriptor.cast()
    }

    fn SetD3DManager(&self, _manager: Ref<IUnknown>) -> Result<()> {
        // Frames are in system memory.
        Ok(())
    }
}

impl IMFGetService_Impl for Source_Impl {
    fn GetService(&self, _service: *const GUID, _riid: *const GUID, _out: *mut *mut core::ffi::c_void) -> Result<()> {
        Err(MF_E_UNSUPPORTED_SERVICE.into())
    }
}

impl IKsControl_Impl for Source_Impl {
    fn KsProperty(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        no_property()
    }
    fn KsMethod(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        no_property()
    }
    fn KsEvent(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        no_property()
    }
}

#[implement(IMFMediaStream2, IKsControl)]
struct Stream {
    source: IMFMediaSource,
    descriptor: IMFStreamDescriptor,
    queue: IMFMediaEventQueue,
    shared: Option<Arc<Shared>>,
    state: Mutex<StreamState>,
}

struct StreamState {
    state: MF_STREAM_STATE,
    /// Latest frame from the service, and the same frame at the size the app picked.
    frame: Vec<u8>,
    frame_size: (usize, usize),
    out: Vec<u8>,
    last_seq: u64,
    /// When the next sample is due.
    due: Instant,
    shutdown: bool,
}

impl Stream {
    fn new(source: IMFMediaSource, descriptor: IMFStreamDescriptor, shared: Option<Arc<Shared>>) -> Result<Self> {
        Ok(Self {
            source,
            descriptor,
            queue: unsafe { MFCreateEventQueue()? },
            shared,
            state: Mutex::new(StreamState {
                state: MF_STREAM_STATE_STOPPED,
                frame: Vec::new(),
                frame_size: (0, 0),
                out: Vec::new(),
                last_seq: 0,
                due: Instant::now(),
                shutdown: false,
            }),
        })
    }

    /// Moves to `value`, telling the pipeline.
    fn set_state(&self, value: MF_STREAM_STATE) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        if st.shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        if st.state == value {
            return Ok(());
        }
        st.state = value;
        let event = if value == MF_STREAM_STATE_RUNNING {
            st.due = Instant::now();
            MEStreamStarted
        } else if value == MF_STREAM_STATE_PAUSED {
            MEStreamPaused
        } else {
            MEStreamStopped
        };
        unsafe {
            self.queue
                .QueueEventParamVar(event.0 as u32, &GUID::zeroed(), HRESULT(0), std::ptr::null())
        }
    }

    /// Stops for good, when the source shuts down.
    fn shutdown(&self) {
        let _ = self.set_state(MF_STREAM_STATE_STOPPED);
        self.state.lock().unwrap().shutdown = true;
        let _ = unsafe { self.queue.Shutdown() };
    }
}

impl IMFMediaEventGenerator_Impl for Stream_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        unsafe { self.queue.GetEvent(flags.0) }
    }
    fn BeginGetEvent(&self, callback: Ref<IMFAsyncCallback>, state: Ref<IUnknown>) -> Result<()> {
        unsafe { self.queue.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }
    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> Result<IMFMediaEvent> {
        unsafe { self.queue.EndGetEvent(result.as_ref()) }
    }
    fn QueueEvent(&self, met: u32, ext: *const GUID, status: HRESULT, value: *const PROPVARIANT) -> Result<()> {
        unsafe { self.queue.QueueEventParamVar(met, ext, status, value) }
    }
}

impl IMFMediaStream_Impl for Stream_Impl {
    fn GetMediaSource(&self) -> Result<IMFMediaSource> {
        Ok(self.source.clone())
    }

    fn GetStreamDescriptor(&self) -> Result<IMFStreamDescriptor> {
        Ok(self.descriptor.clone())
    }

    fn RequestSample(&self, token: Ref<IUnknown>) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        if st.shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        if st.state != MF_STREAM_STATE_RUNNING {
            return Err(MF_E_INVALIDREQUEST.into());
        }
        // One frame per frame time; a new frame from the service is taken as soon as it is there.
        let wait_until = st.due;
        if let Some(sh) = &self.shared {
            while Instant::now() < wait_until && sh.view.latest_seq() == st.last_seq {
                std::thread::sleep(Duration::from_millis(2));
            }
            let StreamState {
                frame,
                frame_size,
                last_seq,
                ..
            } = &mut *st;
            if let Some((w, h, seq)) = sh.view.read(frame) {
                *frame_size = (w, h);
                *last_seq = seq;
            }
        } else {
            let now = Instant::now();
            if now < wait_until {
                std::thread::sleep(wait_until - now);
            }
        }
        st.due = Instant::now() + FRAME_TIME;

        let (w, h) = unsafe {
            let t = self.descriptor.GetMediaTypeHandler()?.GetCurrentMediaType()?;
            let size = t.GetUINT64(&MF_MT_FRAME_SIZE)?;
            ((size >> 32) as usize, (size & 0xffff_ffff) as usize)
        };
        let len = w * h * 3 / 2;
        let StreamState {
            frame, frame_size, out, ..
        } = &mut *st;
        out.resize(len, 0);
        if frame_size.0 > 0 && frame.len() >= frame_size.0 * frame_size.1 * 3 / 2 {
            shared::resize_nv12(frame, frame_size.0, frame_size.1, out, w, h);
        } else {
            // Black until the service sends frames.
            out[..w * h].fill(16);
            out[w * h..].fill(128);
        }
        unsafe {
            let buffer = MFCreateMemoryBuffer(len as u32)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            std::ptr::copy_nonoverlapping(out.as_ptr(), data, len);
            buffer.Unlock()?;
            buffer.SetCurrentLength(len as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(MFGetSystemTime())?;
            sample.SetSampleDuration(FRAME_UNITS)?;
            if let Some(token) = token.as_ref() {
                sample.SetUnknown(&MFSampleExtension_Token, token)?;
            }
            self.queue
                .QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), HRESULT(0), &sample)?;
        }
        Ok(())
    }
}

impl IMFMediaStream2_Impl for Stream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> Result<()> {
        self.set_state(value)
    }

    fn GetStreamState(&self) -> Result<MF_STREAM_STATE> {
        Ok(self.state.lock().unwrap().state)
    }
}

impl IKsControl_Impl for Stream_Impl {
    fn KsProperty(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        no_property()
    }
    fn KsMethod(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        no_property()
    }
    fn KsEvent(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        no_property()
    }
}
