//! Minimal V4L2 capture and output.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::time::Duration;

const BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
const MEMORY_MMAP: u32 = 1;
const FIELD_NONE: u32 = 1;

const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

pub const NV12: u32 = fourcc(b"NV12");

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct PixFormat {
    pub width: u32,
    pub height: u32,
    pub pixelformat: u32,
    pub field: u32,
    pub bytesperline: u32,
    pub sizeimage: u32,
    pub colorspace: u32,
    pub priv_: u32,
    pub flags: u32,
    pub ycbcr_enc: u32,
    pub quantization: u32,
    pub xfer_func: u32,
}

#[repr(C)]
union FormatUnion {
    pix: PixFormat,
    raw: [u8; 200],
    _align: [u64; 25],
}

#[repr(C)]
struct Format {
    type_: u32,
    fmt: FormatUnion,
}

#[repr(C)]
#[derive(Default)]
struct RequestBuffers {
    count: u32,
    type_: u32,
    memory: u32,
    capabilities: u32,
    flags: u8,
    reserved: [u8; 3],
}

#[repr(C)]
#[derive(Default)]
struct Timecode {
    type_: u32,
    flags: u32,
    frames: u8,
    seconds: u8,
    minutes: u8,
    hours: u8,
    userbits: [u8; 4],
}

#[repr(C)]
#[derive(Default)]
struct Buffer {
    index: u32,
    type_: u32,
    bytesused: u32,
    flags: u32,
    field: u32,
    timestamp: libc::timeval,
    timecode: Timecode,
    sequence: u32,
    memory: u32,
    m: u64,
    length: u32,
    reserved2: u32,
    request_fd: i32,
}

impl Default for Format {
    fn default() -> Self {
        Self {
            type_: 0,
            fmt: FormatUnion { raw: [0; 200] },
        }
    }
}

const fn ioc(dir: u32, nr: u32, size: usize) -> libc::c_ulong {
    ((dir << 30) | ((size as u32) << 16) | ((b'V' as u32) << 8) | nr) as libc::c_ulong
}
const W: u32 = 1;
const RW: u32 = 3;

const VIDIOC_G_FMT: libc::c_ulong = ioc(RW, 4, size_of::<Format>());
const VIDIOC_S_FMT: libc::c_ulong = ioc(RW, 5, size_of::<Format>());
const VIDIOC_REQBUFS: libc::c_ulong = ioc(RW, 8, size_of::<RequestBuffers>());
const VIDIOC_QUERYBUF: libc::c_ulong = ioc(RW, 9, size_of::<Buffer>());
const VIDIOC_QBUF: libc::c_ulong = ioc(RW, 15, size_of::<Buffer>());
const VIDIOC_DQBUF: libc::c_ulong = ioc(RW, 17, size_of::<Buffer>());
const VIDIOC_STREAMON: libc::c_ulong = ioc(W, 18, size_of::<i32>());
const VIDIOC_STREAMOFF: libc::c_ulong = ioc(W, 19, size_of::<i32>());

fn ioctl<T>(fd: &OwnedFd, request: libc::c_ulong, arg: &mut T) -> io::Result<()> {
    loop {
        let r = unsafe { libc::ioctl(fd.as_raw_fd(), request, arg as *mut T) };
        if r >= 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

fn open(path: &Path, flags: i32) -> io::Result<OwnedFd> {
    let c = CString::new(path.as_os_str().as_encoded_bytes()).map_err(io::Error::other)?;
    let fd = unsafe { libc::open(c.as_ptr(), flags | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn set_format(fd: &OwnedFd, type_: u32, width: u32, height: u32, pixelformat: u32) -> io::Result<()> {
    let mut f = Format {
        type_,
        ..Default::default()
    };
    ioctl(fd, VIDIOC_G_FMT, &mut f)?;
    let mut pix = unsafe { f.fmt.pix };
    pix.width = width;
    pix.height = height;
    pix.pixelformat = pixelformat;
    pix.field = FIELD_NONE;
    pix.bytesperline = 0;
    pix.sizeimage = 0;
    f.fmt.pix = pix;
    ioctl(fd, VIDIOC_S_FMT, &mut f)?;
    let pix = unsafe { f.fmt.pix };
    if pix.width != width || pix.height != height || pix.pixelformat != pixelformat {
        return Err(io::Error::other(format!(
            "device chose {}x{} {:?}",
            pix.width,
            pix.height,
            pixelformat.to_le_bytes().map(char::from)
        )));
    }
    Ok(())
}

struct Mapped {
    ptr: *mut libc::c_void,
    len: usize,
}

impl Drop for Mapped {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr, self.len) };
    }
}

pub struct Frame<'a> {
    pub data: &'a [u8],
}

/// Memory-mapped streaming capture.
pub struct Capture {
    fd: OwnedFd,
    buffers: Vec<Mapped>,
}

impl Capture {
    pub fn open(path: &Path, width: u32, height: u32, pixelformat: u32) -> io::Result<Self> {
        let fd = open(path, libc::O_RDWR | libc::O_NONBLOCK)?;
        set_format(&fd, BUF_TYPE_VIDEO_CAPTURE, width, height, pixelformat)?;

        let mut req = RequestBuffers {
            count: 4,
            type_: BUF_TYPE_VIDEO_CAPTURE,
            memory: MEMORY_MMAP,
            ..Default::default()
        };
        ioctl(&fd, VIDIOC_REQBUFS, &mut req)?;
        let mut buffers = Vec::new();
        for index in 0..req.count {
            let mut b = Buffer {
                index,
                type_: BUF_TYPE_VIDEO_CAPTURE,
                memory: MEMORY_MMAP,
                ..Default::default()
            };
            ioctl(&fd, VIDIOC_QUERYBUF, &mut b)?;
            let len = b.length as usize;
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd.as_raw_fd(),
                    b.m as libc::off_t,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            buffers.push(Mapped { ptr, len });
            ioctl(&fd, VIDIOC_QBUF, &mut b)?;
        }
        let mut t = BUF_TYPE_VIDEO_CAPTURE as i32;
        ioctl(&fd, VIDIOC_STREAMON, &mut t)?;
        Ok(Self { fd, buffers })
    }

    /// Waits for the next frame and passes it to `f`. The buffer is handed back to the driver after.
    pub fn next<T>(&mut self, timeout: Duration, f: impl FnOnce(Frame<'_>) -> T) -> io::Result<T> {
        let mut pfd = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let r = unsafe { libc::poll(&mut pfd, 1, timeout.as_millis().min(i32::MAX as u128) as i32) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        if r == 0 {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let mut b = Buffer {
            type_: BUF_TYPE_VIDEO_CAPTURE,
            memory: MEMORY_MMAP,
            ..Default::default()
        };
        ioctl(&self.fd, VIDIOC_DQBUF, &mut b)?;
        let mapped = &self.buffers[b.index as usize];
        let used = (b.bytesused as usize).min(mapped.len);
        let data = unsafe { std::slice::from_raw_parts(mapped.ptr as *const u8, used) };
        let out = f(Frame { data });
        ioctl(&self.fd, VIDIOC_QBUF, &mut b)?;
        Ok(out)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let mut t = BUF_TYPE_VIDEO_CAPTURE as i32;
        let _ = ioctl(&self.fd, VIDIOC_STREAMOFF, &mut t);
    }
}

/// Write-based output, for v4l2loopback.
pub struct Output {
    fd: OwnedFd,
}

impl Output {
    pub fn open(path: &Path, width: u32, height: u32, pixelformat: u32) -> io::Result<Self> {
        let fd = open(path, libc::O_RDWR)?;
        set_format(&fd, BUF_TYPE_VIDEO_OUTPUT, width, height, pixelformat)?;
        Ok(Self { fd })
    }

    pub fn write(&mut self, frame: &[u8]) -> io::Result<()> {
        let mut rest = frame;
        while !rest.is_empty() {
            let n = unsafe { libc::write(self.fd.as_raw_fd(), rest.as_ptr().cast(), rest.len()) };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            rest = &rest[n as usize..];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_match_kernel_abi() {
        assert_eq!(size_of::<PixFormat>(), 48);
        assert_eq!(size_of::<Format>(), 208);
        assert_eq!(size_of::<RequestBuffers>(), 20);
        assert_eq!(size_of::<Buffer>(), 88);
    }

    #[test]
    fn ioctl_numbers_match_kernel_headers() {
        assert_eq!(VIDIOC_S_FMT, 0xc0d0_5605);
        assert_eq!(VIDIOC_REQBUFS, 0xc014_5608);
        assert_eq!(VIDIOC_QBUF, 0xc058_560f);
        assert_eq!(VIDIOC_DQBUF, 0xc058_5611);
        assert_eq!(VIDIOC_STREAMON, 0x4004_5612);
    }
}
