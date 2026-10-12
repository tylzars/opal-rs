//! Where the processed video goes, and who is watching it: a v4l2loopback device on Linux, the "Opal C1 Virtual"
//! camera on Windows.

use std::path::PathBuf;

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;

/// Takes the processed frames.
pub struct Output {
    #[cfg(unix)]
    device: opal_v4l2::Output,
    #[cfg(windows)]
    frames: opal_vcam::host::Frames,
    #[cfg(windows)]
    width: usize,
    #[cfg(windows)]
    height: usize,
}

impl Output {
    pub fn write(&mut self, frame: &[u8]) -> std::io::Result<()> {
        #[cfg(unix)]
        return self.device.write(frame);
        #[cfg(windows)]
        {
            self.frames.write(frame, self.width, self.height);
            Ok(())
        }
    }
}

/// Tells which apps are watching the output.
pub struct Watch {
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(windows)]
    frames: opal_vcam::host::Frames,
    /// Keeps the camera registered with Windows while the session runs.
    #[cfg(windows)]
    _camera: opal_vcam::host::VirtualCamera,
}

impl Watch {
    /// Names of the apps using the output, one per app.
    pub fn users(&mut self) -> Vec<String> {
        #[cfg(unix)]
        return users_of(&self.path);
        #[cfg(windows)]
        {
            if !self.frames.streaming() {
                return Vec::new();
            }
            // Windows does not say which app took this camera; name the ones using any camera now.
            let names = crate::platform::camera_apps();
            if names.is_empty() { vec!["app".into()] } else { names }
        }
    }
}

/// Opens the output for `width` x `height` NV12 video, showing black until frames come. On Linux `path` is the
/// v4l2loopback device and nothing is opened without one; on Windows the virtual camera is always used.
pub fn open(path: Option<PathBuf>, width: usize, height: usize) -> Result<Option<(Output, Watch)>> {
    #[cfg(unix)]
    {
        let Some(path) = path else { return Ok(None) };
        let path = path.canonicalize().with_context(|| path.display().to_string())?;
        let mut device = opal_v4l2::Output::open(&path, width as u32, height as u32, opal_v4l2::NV12)
            .with_context(|| path.display().to_string())?;
        device.write(&black_frame(width, height))?;
        Ok(Some((Output { device }, Watch { path })))
    }
    #[cfg(windows)]
    {
        let _ = path;
        let camera = opal_vcam::host::VirtualCamera::create().map_err(anyhow::Error::msg)?;
        let mut frames = opal_vcam::host::Frames::new();
        frames.write(&black_frame(width, height), width, height);
        Ok(Some((
            Output { frames, width, height },
            Watch {
                frames: opal_vcam::host::Frames::new(),
                _camera: camera,
            },
        )))
    }
}

/// Black NV12 frame, written first so apps can open the output before video arrives.
pub fn black_frame(width: usize, height: usize) -> Vec<u8> {
    let y = width * height;
    let mut f = vec![0u8; y];
    f.resize(y + y / 2, 128);
    f
}

/// In low power mode apps use the camera's own USB video. Returns how many apps use it and their names. Windows
/// cannot tell, so there the camera counts as in use while the service runs.
pub fn low_power_users(node: &mut Option<PathBuf>) -> (usize, Vec<String>) {
    #[cfg(unix)]
    {
        if node.as_ref().is_none_or(|n| !n.exists()) {
            *node = find_camera_node();
        }
        let names = node.as_deref().map_or_else(Vec::new, users_of);
        (names.len(), names)
    }
    #[cfg(windows)]
    {
        let _ = node;
        (1, crate::platform::camera_apps())
    }
}

/// The camera's own video device, when it is plugged in and booted.
#[cfg(unix)]
fn find_camera_node() -> Option<PathBuf> {
    std::fs::read_dir("/sys/class/video4linux")
        .ok()?
        .flatten()
        .find_map(|e| {
            let dir = e.path();
            let read = |f: &str| std::fs::read_to_string(dir.join(f)).map(|s| s.trim().to_owned());
            // `device` is the USB interface; its parent is the USB device.
            let ours = read("index").is_ok_and(|i| i == "0")
                && read("device/../idVendor").is_ok_and(|v| v == "03e7")
                && read("device/../idProduct").is_ok_and(|p| p == "f63b");
            ours.then(|| PathBuf::from("/dev").join(e.file_name()))
        })
}

/// Names of the programs that have `path` open, one entry per process.
#[cfg(unix)]
fn users_of(path: &std::path::Path) -> Vec<String> {
    let me = std::process::id().to_string();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    procs
        .flatten()
        .filter(|p| {
            let name = p.file_name();
            let name = name.to_string_lossy();
            name.bytes().all(|b| b.is_ascii_digit()) && name != me
        })
        .filter(|p| {
            std::fs::read_dir(p.path().join("fd")).is_ok_and(|fds| {
                fds.flatten()
                    .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|t| t == path))
            })
        })
        .map(|p| {
            std::fs::read_to_string(p.path().join("comm"))
                .map_or_else(|_| "?".into(), |c| c.trim().replace(char::is_whitespace, "_"))
        })
        .collect()
}
