//! Where the video goes and who watches it: v4l2loopback on Linux, "Opal C1 Virtual" on Windows.

use std::path::PathBuf;

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;

#[cfg(unix)]
use crate::cameras::{self, Camera};

/// Takes the processed frames and tells which apps watch them. Kept from one camera session to the next, so apps
/// keep the camera when it restarts, at a new resolution too.
pub struct Outputs {
    /// The size of the session's frames, which cameras are opened at.
    width: usize,
    height: usize,
    /// Frames resized for a camera that an app holds at another size.
    #[cfg(unix)]
    scaled: Vec<u8>,
    #[cfg(unix)]
    main: Camera,
    #[cfg(unix)]
    sinks: Vec<Sink>,
    /// Set while the cameras are handed back for the module to reload; nothing is reopened meanwhile.
    #[cfg(unix)]
    released: bool,
    /// Cameras that would not open, so the failure is reported once.
    #[cfg(unix)]
    failed: Vec<u32>,
    #[cfg(windows)]
    frames: opal_vcam::host::Frames,
    #[cfg(windows)]
    watch: opal_vcam::host::Frames,
}

/// Keeps the Windows camera registered while the service runs; held by the thread that made it.
pub struct Registration {
    #[cfg(windows)]
    _camera: opal_vcam::host::VirtualCamera,
}

/// One v4l2loopback device and the apps that have it open.
#[cfg(unix)]
struct Sink {
    camera: Camera,
    device: opal_v4l2::Output,
    /// The size it was opened at. v4l2loopback keeps it while an app has the camera open.
    size: (usize, usize),
    users: Vec<String>,
}

impl Outputs {
    /// Sends a `width` x `height` frame to the main camera, and to each extra one an app is using. The main camera
    /// always gets it, so an app opening it starts with the live picture rather than black.
    pub fn write(&mut self, frame: &[u8], width: usize, height: usize) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            let mut result = Ok(());
            let Self {
                main, sinks, scaled, ..
            } = self;
            for s in sinks
                .iter_mut()
                .filter(|s| s.camera.nr == main.nr || !s.users.is_empty())
            {
                let data = if s.size == (width, height) {
                    frame
                } else {
                    let (w, h) = s.size;
                    scaled.resize(w * h * 3 / 2, 0);
                    opal_vcam::shared::resize_nv12(frame, width, height, scaled, w, h);
                    &scaled[..]
                };
                if let Err(e) = s.device.write(data) {
                    result = Err(e);
                }
            }
            result
        }
        #[cfg(windows)]
        {
            // The camera resizes frames to what each app asked for.
            self.frames.write(frame, width, height);
            Ok(())
        }
    }

    /// Takes the size of a new session's frames. Cameras no app holds are reopened at it now, the others once
    /// their apps let go.
    pub fn set_size(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.refresh();
    }

    /// Shows black while the camera restarts, rather than a frozen picture.
    pub fn blank(&mut self) {
        #[cfg(unix)]
        for s in &mut self.sinks {
            let _ = s.device.write(&black_frame(s.size.0, s.size.1));
        }
        #[cfg(windows)]
        self.frames
            .write(&black_frame(self.width, self.height), self.width, self.height);
    }

    /// The devices whose users `scan_users` should find.
    pub fn watch_paths(&self) -> Vec<PathBuf> {
        #[cfg(unix)]
        return self.sinks.iter().map(|s| s.camera.path()).collect();
        #[cfg(windows)]
        Vec::new()
    }

    /// Takes what `scan_users` found for `watch_paths`, which may be out of date by now.
    pub fn set_users(&mut self, paths: &[PathBuf], found: Vec<Vec<String>>) {
        #[cfg(unix)]
        for (path, users) in paths.iter().zip(found) {
            if let Some(s) = self.sinks.iter_mut().find(|s| s.camera.path() == *path) {
                s.users = users;
            }
        }
        #[cfg(windows)]
        let _ = (paths, found);
    }

    /// Names of the apps using any camera, one per app and camera.
    pub fn users(&mut self) -> Vec<String> {
        #[cfg(unix)]
        return self.sinks.iter().flat_map(|s| s.users.clone()).collect();
        #[cfg(windows)]
        {
            if !self.watch.streaming() {
                return Vec::new();
            }
            // Windows does not say which app took this camera; name the ones using any camera now.
            let names = crate::platform::camera_apps();
            if names.is_empty() { vec!["app".into()] } else { names }
        }
    }

    /// Closes every camera so the module can reload, until `reopen`.
    pub fn release(&mut self) {
        #[cfg(unix)]
        {
            self.sinks.clear();
            self.released = true;
        }
    }

    pub fn reopen(&mut self) {
        #[cfg(unix)]
        {
            self.released = false;
            self.refresh();
        }
    }

    /// Opens the cameras that are set up and present but not open yet, and closes ones that went away. A camera
    /// that was removed but is still there (until the module reloads) keeps its video while an app uses it. One
    /// left at another size is reopened at the current one once no app uses it.
    pub fn refresh(&mut self) {
        #[cfg(unix)]
        {
            if self.released {
                return;
            }
            let wanted = self.wanted();
            let size = (self.width, self.height);
            // Closed first: v4l2loopback keeps the size while any writer has the camera open.
            let mut resize = Vec::new();
            self.sinks.retain(|s| {
                let keep = s.camera.present() && (wanted.contains(&s.camera) || !s.users.is_empty());
                if keep && s.size != size && s.users.is_empty() {
                    resize.push(s.camera.clone());
                    return false;
                }
                keep
            });
            for camera in resize {
                // An app that opened it since the last look keeps the old size.
                match open_device(&camera.path(), size.0, size.1) {
                    Ok((device, size)) => self.sinks.push(Sink {
                        camera,
                        device,
                        size,
                        users: Vec::new(),
                    }),
                    Err(e) => eprintln!("{}: {e:#}", camera.name),
                }
            }
            for camera in wanted {
                if self.sinks.iter().any(|s| s.camera == camera) || !camera.present() {
                    continue;
                }
                match open_device(&camera.path(), size.0, size.1) {
                    Ok((device, size)) => {
                        self.failed.retain(|n| *n != camera.nr);
                        self.sinks.push(Sink {
                            camera,
                            device,
                            size,
                            users: Vec::new(),
                        })
                    }
                    Err(e) if !self.failed.contains(&camera.nr) => {
                        eprintln!("{}: {e:#}", camera.name);
                        self.failed.push(camera.nr);
                    }
                    Err(_) => {}
                }
            }
        }
    }

    /// The main camera, then the extra ones set up for v4l2loopback.
    #[cfg(unix)]
    fn wanted(&self) -> Vec<Camera> {
        let mut all = vec![self.main.clone()];
        all.extend(cameras::configured().into_iter().filter(|c| c.nr != self.main.nr));
        all
    }
}

/// One line per camera for the control panel, joined with " | ": `<number> <state> <apps|-> <name>`. The state
/// is `on`, `off`, `restart` (set up, appears after the computer restarts) or `leaving` (removed, but an app still
/// uses it until the computer restarts).
pub fn describe(outputs: Option<&Outputs>) -> String {
    #[cfg(unix)]
    {
        let main = outputs.map(|o| o.main.clone());
        let mut all: Vec<Camera> = main.iter().cloned().collect();
        all.extend(
            cameras::configured()
                .into_iter()
                .filter(|c| main.as_ref().is_none_or(|m| m.nr != c.nr)),
        );
        // Removed ones still there: one an app still uses, or one waiting for the module to reload.
        let mut leaving: Vec<Camera> = outputs
            .map(|o| {
                o.sinks
                    .iter()
                    .map(|s| s.camera.clone())
                    .filter(|c| !all.contains(c))
                    .collect()
            })
            .unwrap_or_default();
        for c in cameras::leftovers(&all) {
            if !leaving.contains(&c) {
                leaving.push(c);
            }
        }
        let wanted = all.len();
        all.extend(leaving);
        all.iter()
            .enumerate()
            .map(|(i, c)| {
                let sink = outputs.and_then(|o| o.sinks.iter().find(|s| s.camera == *c));
                let state = match sink {
                    _ if i >= wanted => "leaving",
                    Some(_) => "on",
                    None if c.present() => "off",
                    None => "restart",
                };
                let mut apps: Vec<&str> = sink
                    .map(|s| s.users.iter().map(String::as_str).collect())
                    .unwrap_or_default();
                apps.sort();
                apps.dedup();
                let apps = if apps.is_empty() { "-".into() } else { apps.join(",") };
                format!("{} {state} {apps} {}", c.nr, c.name)
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }
    #[cfg(windows)]
    {
        let _ = outputs;
        String::new()
    }
}

/// Opens the outputs for `width` x `height` NV12 video, showing black until frames come. On Linux `path` is the
/// main v4l2loopback device and nothing is opened without one; on Windows the virtual camera is always used.
pub fn open(path: Option<PathBuf>, width: usize, height: usize) -> Result<Option<(Outputs, Registration)>> {
    #[cfg(unix)]
    {
        let Some(path) = path else { return Ok(None) };
        let path = path.canonicalize().with_context(|| path.display().to_string())?;
        let main = camera_at(&path).with_context(|| format!("{} is not a video device", path.display()))?;
        let (device, size) = open_device(&path, width, height)?;
        let mut outputs = Outputs {
            width,
            height,
            scaled: Vec::new(),
            sinks: vec![Sink {
                camera: main.clone(),
                device,
                size,
                users: Vec::new(),
            }],
            main,
            released: false,
            failed: Vec::new(),
        };
        outputs.refresh();
        Ok(Some((outputs, Registration {})))
    }
    #[cfg(windows)]
    {
        let _ = path;
        let camera = opal_vcam::host::VirtualCamera::create().map_err(anyhow::Error::msg)?;
        let mut frames = opal_vcam::host::Frames::new();
        frames.write(&black_frame(width, height), width, height);
        Ok(Some((
            Outputs {
                width,
                height,
                frames,
                watch: opal_vcam::host::Frames::new(),
            },
            Registration { _camera: camera },
        )))
    }
}

/// The camera at `/dev/videoN`, named as the device is now.
#[cfg(unix)]
fn camera_at(path: &std::path::Path) -> Option<Camera> {
    let nr = path.file_name()?.to_str()?.strip_prefix("video")?.parse().ok()?;
    let name = std::fs::read_to_string(format!("/sys/class/video4linux/video{nr}/name")).ok()?;
    Some(Camera {
        nr,
        name: name.trim().to_owned(),
    })
}

/// Opens a v4l2loopback device and writes black, which makes it show up in apps. An app holding it at another
/// size keeps that size; returns the size in use.
#[cfg(unix)]
fn open_device(path: &std::path::Path, width: usize, height: usize) -> Result<(opal_v4l2::Output, (usize, usize))> {
    let (mut device, (w, h)) = opal_v4l2::Output::open(path, width as u32, height as u32, opal_v4l2::NV12)
        .with_context(|| path.display().to_string())?;
    let size = (w as usize, h as usize);
    device.write(&black_frame(size.0, size.1))?;
    Ok((device, size))
}

/// Black NV12 frame, written first so apps can open the output before video arrives.
pub fn black_frame(width: usize, height: usize) -> Vec<u8> {
    let y = width * height;
    let mut f = vec![0u8; y];
    f.resize(y + y / 2, 128);
    f
}

/// When apps use the camera's own USB video (low power mode on Linux). Returns how many apps use it and their names,
/// so the camera runs only while one does, as the camera's own app has it: an app closing the video stops the camera,
/// and the next one to open it needs it started again. Windows says which apps use a camera, not which one.
pub fn low_power_users(node: &mut Option<PathBuf>) -> (usize, Vec<String>) {
    #[cfg(unix)]
    {
        if node.as_ref().is_none_or(|n| !n.exists()) {
            *node = find_camera_node();
        }
        let names = node
            .as_ref()
            .map_or_else(Vec::new, |n| scan_users(std::slice::from_ref(n)).remove(0));
        (names.len(), names)
    }
    #[cfg(windows)]
    {
        let _ = node;
        let names = crate::platform::camera_apps();
        (names.len(), names)
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

/// For each of `paths`, the names of the programs that have it open, one entry per process. One pass over /proc.
#[cfg_attr(windows, allow(unused_mut))]
pub fn scan_users(paths: &[PathBuf]) -> Vec<Vec<String>> {
    let mut found = vec![Vec::new(); paths.len()];
    #[cfg(unix)]
    {
        if paths.is_empty() {
            return found;
        }
        let me = std::process::id().to_string();
        let Ok(procs) = std::fs::read_dir("/proc") else {
            return found;
        };
        for p in procs.flatten() {
            let pid = p.file_name();
            let pid = pid.to_string_lossy();
            if !pid.bytes().all(|b| b.is_ascii_digit()) || pid == me {
                continue;
            }
            let Ok(fds) = std::fs::read_dir(p.path().join("fd")) else {
                continue;
            };
            let mut has = vec![false; paths.len()];
            for fd in fds.flatten() {
                if let Ok(target) = std::fs::read_link(fd.path())
                    && let Some(i) = paths.iter().position(|q| *q == target)
                {
                    has[i] = true;
                }
            }
            if has.iter().any(|h| *h) {
                let name = std::fs::read_to_string(p.path().join("comm"))
                    .map_or_else(|_| "?".into(), |c| c.trim().replace(char::is_whitespace, "_"));
                for (i, _) in has.iter().enumerate().filter(|(_, h)| **h) {
                    found[i].push(name.clone());
                }
            }
        }
    }
    found
}
