//! Frame buffer shared by the service and the media source: a header, then one NV12 frame.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

/// Name of the shared memory. The source creates it in the global namespace, as Frame Server runs it outside the
/// user's session, and the service in the user's session opens it there.
pub const NAME: &str = "Global\\opal-rs-vcam";

pub const MAGIC: u32 = u32::from_le_bytes(*b"OPVC");
pub const VERSION: u32 = 2;
pub const MAX_WIDTH: usize = 3840;
pub const MAX_HEIGHT: usize = 2160;
pub const HEADER_SIZE: usize = 64;
/// Bytes to map: the header and the largest frame.
pub const SIZE: usize = HEADER_SIZE + MAX_WIDTH * MAX_HEIGHT * 3 / 2;

#[repr(C)]
pub struct Header {
    pub magic: AtomicU32,
    pub version: AtomicU32,
    pub width: AtomicU32,
    pub height: AtomicU32,
    /// Odd while a frame is being written.
    pub seq: AtomicU64,
    /// Counted up by the source each time an app asks for a frame. A count that stopped moving means no app is
    /// watching, even if the source went away without saying so.
    pub pulls: AtomicU32,
}

const _: () = assert!(std::mem::size_of::<Header>() <= HEADER_SIZE);

/// A mapped buffer of [`SIZE`] bytes.
pub struct View {
    base: *mut u8,
}

// The header is atomics and the frame is guarded by the sequence count.
unsafe impl Send for View {}

impl View {
    /// # Safety
    /// `base` must point to [`SIZE`] bytes, 8-byte aligned, that stay mapped while the view is used.
    pub unsafe fn new(base: *mut u8) -> Self {
        Self { base }
    }

    pub fn header(&self) -> &Header {
        unsafe { &*(self.base as *const Header) }
    }

    /// Sets up the buffer for this version. One left by an earlier run of this version is kept as it is; one of
    /// another version (a service or camera from before an update, still running) is taken over.
    pub fn init(&self) {
        let h = self.header();
        if h.magic.load(Ordering::Acquire) == MAGIC && h.version.load(Ordering::Acquire) == VERSION {
            return;
        }
        h.version.store(VERSION, Ordering::Release);
        h.magic.store(MAGIC, Ordering::Release);
    }

    pub fn valid(&self) -> bool {
        let h = self.header();
        h.magic.load(Ordering::Acquire) == MAGIC && h.version.load(Ordering::Acquire) == VERSION
    }

    fn frame_ptr(&self) -> *mut u8 {
        unsafe { self.base.add(HEADER_SIZE) }
    }

    /// Writes one NV12 frame. Frames larger than the buffer are ignored.
    pub fn write(&self, frame: &[u8], width: usize, height: usize) {
        let len = width * height * 3 / 2;
        if !frame_size_ok(width, height) || frame.len() < len {
            return;
        }
        let h = self.header();
        // Odd while writing. Taken from the count rather than added to it, so a writer that died half way does
        // not leave the count odd for good.
        let odd = h.seq.load(Ordering::Relaxed) | 1;
        h.seq.store(odd, Ordering::Relaxed);
        fence(Ordering::Release);
        h.width.store(width as u32, Ordering::Relaxed);
        h.height.store(height as u32, Ordering::Relaxed);
        unsafe { std::ptr::copy_nonoverlapping(frame.as_ptr(), self.frame_ptr(), len) };
        h.seq.store(odd.wrapping_add(1), Ordering::Release);
    }

    /// Copies the latest frame into `out` and returns its size and sequence count, unless none was written yet or
    /// one is being written now.
    pub fn read(&self, out: &mut Vec<u8>) -> Option<(usize, usize, u64)> {
        let h = self.header();
        let before = h.seq.load(Ordering::Acquire);
        if before == 0 || before % 2 == 1 {
            return None;
        }
        let (w, hgt) = (
            h.width.load(Ordering::Relaxed) as usize,
            h.height.load(Ordering::Relaxed) as usize,
        );
        if !frame_size_ok(w, hgt) {
            return None;
        }
        let len = w * hgt * 3 / 2;
        out.resize(len, 0);
        unsafe { std::ptr::copy_nonoverlapping(self.frame_ptr(), out.as_mut_ptr(), len) };
        // Keeps the copy above before the check below.
        fence(Ordering::Acquire);
        (h.seq.load(Ordering::Relaxed) == before).then_some((w, hgt, before))
    }

    pub fn latest_seq(&self) -> u64 {
        self.header().seq.load(Ordering::Acquire)
    }

    /// Notes that an app asked for a frame.
    pub fn pull(&self) {
        self.header().pulls.fetch_add(1, Ordering::Relaxed);
    }

    pub fn pulls(&self) -> u32 {
        self.header().pulls.load(Ordering::Relaxed)
    }
}

/// An NV12 size that fits the buffer: even, at least 2 x 2.
fn frame_size_ok(w: usize, h: usize) -> bool {
    (2..=MAX_WIDTH).contains(&w) && (2..=MAX_HEIGHT).contains(&h) && w.is_multiple_of(2) && h.is_multiple_of(2)
}

/// Nearest-pixel resize of an NV12 frame, for apps that ask for another size than the service sends. Sizes that
/// do not fit the buffers leave `dst` as it is.
pub fn resize_nv12(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize) {
    let fits = |b: &[u8], w: usize, h: usize| frame_size_ok(w, h) && b.len() >= w * h * 3 / 2;
    if !fits(src, sw, sh) || !fits(dst, dw, dh) {
        return;
    }
    if sw == dw && sh == dh {
        dst[..dw * dh * 3 / 2].copy_from_slice(&src[..sw * sh * 3 / 2]);
        return;
    }
    for y in 0..dh {
        let sy = y * sh / dh;
        let (s, d) = (&src[sy * sw..][..sw], &mut dst[y * dw..][..dw]);
        for (x, p) in d.iter_mut().enumerate() {
            *p = s[x * sw / dw];
        }
    }
    let (src_uv, dst_uv) = (&src[sw * sh..], &mut dst[dw * dh..]);
    for y in 0..dh / 2 {
        let sy = y * (sh / 2) / (dh / 2);
        for x in 0..dw / 2 {
            let sx = x * (sw / 2) / (dw / 2);
            dst_uv[y * dw + 2 * x] = src_uv[sy * sw + 2 * sx];
            dst_uv[y * dw + 2 * x + 1] = src_uv[sy * sw + 2 * sx + 1];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer() -> Vec<u64> {
        vec![0u64; SIZE.div_ceil(8)]
    }

    #[test]
    fn write_then_read() {
        let mut mem = buffer();
        let view = unsafe { View::new(mem.as_mut_ptr() as *mut u8) };
        view.init();
        assert!(view.valid());
        let mut out = Vec::new();
        assert!(view.read(&mut out).is_none(), "nothing written yet");
        let frame: Vec<u8> = (0..64 * 32 * 3 / 2).map(|i| i as u8).collect();
        view.write(&frame, 64, 32);
        let (w, h, seq) = view.read(&mut out).unwrap();
        assert_eq!((w, h, seq), (64, 32, 2));
        assert_eq!(out, frame);
        assert_eq!(view.pulls(), 0);
        view.pull();
        assert_eq!(view.pulls(), 1);
        view.init();
        assert_eq!(view.latest_seq(), 2, "an existing buffer of this version is kept");
        // One of another version is taken over.
        view.header().version.store(1, std::sync::atomic::Ordering::Relaxed);
        assert!(!view.valid());
        view.init();
        assert!(view.valid());
    }

    #[test]
    fn writer_that_died_half_way_is_recovered() {
        let mut mem = buffer();
        let view = unsafe { View::new(mem.as_mut_ptr() as *mut u8) };
        view.init();
        // As if a service was killed between marking a frame and finishing it.
        view.header().seq.store(5, std::sync::atomic::Ordering::Relaxed);
        let mut out = Vec::new();
        assert!(view.read(&mut out).is_none());
        view.write(&[9; 6], 2, 2);
        assert_eq!(view.read(&mut out), Some((2, 2, 6)));
    }

    #[test]
    fn odd_or_tiny_sizes_are_refused() {
        let mut mem = buffer();
        let view = unsafe { View::new(mem.as_mut_ptr() as *mut u8) };
        view.write(&[0; 6], 1, 1);
        view.write(&[0; 30], 5, 4);
        let mut out = Vec::new();
        assert!(view.read(&mut out).is_none());
        let mut dst = vec![7u8; 1920 * 1080 * 3 / 2];
        resize_nv12(&[0; 1], 1, 1, &mut dst, 1920, 1080);
        resize_nv12(&[0; 6], 2, 2, &mut dst, 4000, 4000);
        assert!(dst.iter().all(|&b| b == 7));
    }

    #[test]
    fn half_written_frames_are_not_read() {
        let mut mem = buffer();
        let view = unsafe { View::new(mem.as_mut_ptr() as *mut u8) };
        view.init();
        view.write(&[7u8; 96], 8, 8);
        view.header().seq.fetch_add(1, Ordering::AcqRel);
        assert!(view.read(&mut Vec::new()).is_none());
    }

    #[test]
    fn resize_keeps_flat_colour() {
        let (sw, sh, dw, dh) = (8, 4, 4, 2);
        let mut src = vec![50u8; sw * sh];
        src.extend([90u8, 160].repeat(sw * sh / 4));
        let mut dst = vec![0u8; dw * dh * 3 / 2];
        resize_nv12(&src, sw, sh, &mut dst, dw, dh);
        assert!(dst[..dw * dh].iter().all(|&p| p == 50));
        assert_eq!(&dst[dw * dh..], [90, 160, 90, 160]);
    }
}
