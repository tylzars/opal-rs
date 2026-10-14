# Developing opal-rs

How to build opal-rs from scratch, how the code is laid out, and how to build the release packages.

## What you need
- Rust 1.89 or newer, from [rustup.rs](https://rustup.rs).
- Your own copy of `Opal_Composer_2.0.0_24.dmg`, from Opal's download page, at `assets/` (see
  [assets/README.md](assets/README.md)). Nothing from it is ever committed: `.gitignore` keeps it out.
- Linux: `v4l2loopback` (`sudo apt install v4l2loopback-dkms` if your kernel lacks it), and for the control panel
  the usual desktop libraries (on Ubuntu these are installed already).
- For the Windows build from Linux: `rustup target add x86_64-pc-windows-gnu` and
  `sudo apt install gcc-mingw-w64-x86-64`.
- For the Ubuntu/Debian package: `cargo install cargo-deb`.

## Build and run on Linux
```shell
cargo build --release
target/release/opal-rs assets extract          # copies the camera files out of the disk image, checks each SHA-256

# Camera access without root, and the "Opal C1 Virtual" camera now and at every start:
sudo cp dist/70-opal-c1.rules /etc/udev/rules.d/ && sudo udevadm control --reload && sudo udevadm trigger
sudo cp dist/v4l2loopback.conf /etc/modprobe.d/
echo v4l2loopback | sudo tee /etc/modules-load.d/v4l2loopback.conf
sudo modprobe v4l2loopback

target/release/opal-rs run --output /dev/video10   # the service, in the foreground; Ctrl-C stops it
target/release/opal-rs-gui                         # the control panel, in another terminal
```
To install it for your user instead: `cargo install --path crates/cli` (and `crates/gui`, `crates/tray`), then
`cp dist/opal-rs.service ~/.config/systemd/user/ && systemctl --user daemon-reload`, and
`cp dist/opal-rs-tray.desktop ~/.config/autostart/` for the tray icon, which starts the service at sign-in.

## Build and run on Windows
The simplest is to build the installer (below) and run it. By hand:
1. `cargo build --release --target x86_64-pc-windows-gnu -p opal-rs -p opal-gui -p opal-tray -p opal-vcam`
2. Copy `opal-rs.exe`, `opal-rs-gui.exe`, `opal-rs-tray.exe` and `opal_vcam.dll` from
   `target/x86_64-pc-windows-gnu/release` into `C:\Program Files\opal-rs`. It has to be a folder like this one:
   Windows' camera service loads `opal_vcam.dll` and cannot read user folders.
3. In that folder, in a PowerShell opened as administrator: `regsvr32 opal_vcam.dll`.
4. Start `opal-rs-tray.exe` (it starts the service) and `opal-rs-gui.exe` (it asks for the disk image).

Windows installs the WinUSB driver for the camera by itself. Only if `opal-rs list` shows no camera, install WinUSB
for interface 0 with [Zadig](https://zadig.akeo.ie/), leaving the video and audio interfaces alone.

## Release packages
The package builder is part of the workspace (`crates/xtask`, run through the alias in `.cargo/config.toml`):
```shell
cargo xtask deb     # target/dist/opal-rs_<version>-1_amd64.deb
cargo xtask exe     # target/dist/opal-rs-setup.exe, with the four programs built in
cargo xtask all     # both
```
- Packaged builds leave out the source tree's `assets/` folder: installed copies keep the camera files in the
  user's data folder, and neither package holds anything from Opal.
- Paths of the build machine are stripped from the programs.
- The .deb's install scripts are in `dist/deb/`; its contents are listed under `[package.metadata.deb]` in
  `crates/cli/Cargo.toml`.
- The version comes from `[workspace.package]` in `Cargo.toml`.
- GitHub builds both on every push to main and every pull request (`.github/workflows/build.yml`), after the format, lint and test checks below,
  and keeps them as a download on the run. Pushing a tag such as `v1.0.0` also publishes them as a GitHub release.

## Tests and checks
```shell
cargo test --release            # with the disk image in assets/, also extracts and checks every camera file
cargo clippy --workspace --all-targets
cargo clippy --release --target x86_64-pc-windows-gnu -p opal-rs -p opal-gui -p opal-tray -p opal-vcam -p opal-setup
cargo fmt --all --check
```

## How it fits together
```
camera  --USB-->  opal-rs (service)  --frames-->  "Opal C1 Virtual" (v4l2loopback / Windows virtual camera)  -->  apps
                     ^   control channel (ctl, control panel, tray)
```
The service boots the camera's firmware into its RAM (never its flash), builds a pipeline on the camera (video,
person segmentation, face and hand detection, microphone), takes the video over USB, applies the effects and hands
the frames to the virtual camera. It runs the camera only while an app watches.

| Crate | What it is |
| --- | --- |
| `xlink` | The camera's USB transport: streams, flow control, the bootloader. |
| `dai` | The camera's application protocol: boot, RPC, pipelines, messages. |
| `c1` | What is specific to the Opal C1: its pipeline, controls, face and hand decoding. |
| `assets` | Reads the Opal Composer disk image (UDIF, HFS Plus, xz) and extracts the camera files. |
| `effects` | Video effects on NV12 frames: blur, looks, framing, stickers and the rest. |
| `cli` | `opal-rs`: the service (`daemon.rs`), `ctl`, `cameras`, `uninstall` and friends. |
| `ipc` | The control channel between the service and its clients. |
| `v4l2` | Linux video devices, for v4l2loopback and the camera's own video. |
| `vcam` | The Windows virtual camera: a Media Foundation source DLL and the service side. |
| `gui` | The control panel (egui). |
| `tray` | The tray icon. |
| `setup` | The Windows installer. |
| `xtask` | Builds the release packages. |

[docs/c1-protocol.md](docs/c1-protocol.md) has notes on the camera's USB interface.

## Debugging
- The service log: `journalctl --user -u opal-rs -f` on Linux, `%LOCALAPPDATA%\opal-rs\service.log` on Windows.
- `opal-rs ctl status` shows the camera's state; `opal-rs list` shows the camera's USB state.
- `OPAL_RS_LOG=debug opal-rs run ...` shows the camera's own log too.
- When the camera stops answering, the log shows the last packets each way and how full each stream was.
- `OPAL_RS_ASSETS=<folder>` points the service at another folder of camera files.

## Rules of the project
- Nothing from Opal goes into the repository or the packages: no firmware, networks, tuning data or code.
  The camera files only ever come from the user's own download.
- Never write to the camera's flash or bootloader settings; only boot into RAM.
- A restart that was asked for (a new resolution or power mode) ends the session with an XLink reset and boots the
  camera's bootloader straight away, as the camera's own app does. When the service stops, its firmware is left
  running instead (the camera's own firmware does not always start cleanly after a reset), and the next start
  takes it over. Other boots are at least a minute apart, and after three failed restarts in ten minutes the
  service waits for the camera to be plugged in again. Never boot a running camera that is on a USB 2 connection.
  Keep all of this when changing the boot or restart code.
- Text files are plain ASCII. Comments and messages are short and plain.
