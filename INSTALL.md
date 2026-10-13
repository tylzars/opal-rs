# Installing and using opal-rs

## Install

### Ubuntu and Debian
Ubuntu 24.04, Debian 13 or newer. Other Linux systems: see [DEVELOPING.md](DEVELOPING.md).
```shell
sudo apt install ./opal-rs_1.0.0-1_amd64.deb
```
Then log out and back in (or restart, after installing `v4l2loopback-dkms`). The camera service and the tray
icon start by themselves.

The "Opal C1 Virtual" camera uses the kernel's `v4l2loopback` module, which Ubuntu 24.04 includes. If the install
says the kernel has none, also install `v4l2loopback-dkms`; with Secure Boot on, that asks for a password to be set
and confirmed once after a restart. If the install says v4l2loopback is already loaded with other settings, restart.

### Windows
Run `opal-rs-setup.exe` and allow it to make changes. It installs into `C:\Program Files\opal-rs`, sets up the
"Opal C1 Virtual" camera, starts the tray icon at sign-in, adds "Opal C1 Control" to the Start menu and offers
to start right away.

Windows 11 is needed. On Windows 10, which has no virtual cameras, Opal C1 works as a plain webcam without opal-rs.

## First run
Open **Opal C1 Control** (from the app menu or the Start menu). The first time, it asks for the Opal Composer disk
image, which holds the files the camera needs:
1. **Open Opal's download page** and download Opal Composer 2.0 (`Opal_Composer_2.0.0_24.dmg`).
2. **Choose the file** that was downloaded. It can be deleted afterwards.

## Everyday use
- In applications, choose **"Opal C1 Virtual"**. Applications also list the camera's own "Opal C1", which gives
  no video while opal-rs runs the camera (except in low power mode on Linux).
- The camera runs while an app uses it and stops shortly after the last one closes, and while the screen is
  locked. Its light is white while it sends video.
- **Opal C1 Control** shows a preview and has every setting. The **tray icon** shows when the camera is on, turns
  the video off and on, and opens the control panel.
- **Low power mode** (Linux, in the control panel) makes the camera a plain webcam with no effects, for less
  processing. Choose "Opal C1" in applications while it is on. Changing it restarts the camera.
- Settings are kept between sessions. Presets keep a look with its lighting and effects under a name.

### Several apps at once (Linux)
On Linux an app takes a camera for itself: while one app uses "Opal C1 Virtual", another cannot. Add a camera for
each extra app in the control panel (Virtual cameras), or:
```shell
opal-rs cameras                 # list them and which app uses each
opal-rs cameras add "Zoom"      # asks for the password
opal-rs cameras remove "Zoom"
opal-rs cameras apply           # apply changes that waited for the cameras to be free
```
All of them show the same video and effects. Changes take effect when no app uses an Opal camera, or at the next
restart. Up to four extra cameras, named with letters, digits, spaces and `- _ . ( )`. Windows shares one camera
between apps itself.

## Command line
Everything in the control panel can also be done with `opal-rs ctl <command>` while the service runs.

| Command | What it does |
| --- | --- |
| `status` | Show the camera's state |
| `on`, `off`, `auto` | Force video on or off, or follow apps (the default) |
| `video off`, `video on` | Show the camera-off picture instead of the video |
| `resolution 1080p` | `720p`, `1080p`, `1440p` or `2160p`; restarts the camera |
| `power low`, `power normal` | Low power mode on or off (Linux); restarts the camera |
| `focus 120`, `af` | Manual focus (0 to 255) or autofocus |
| `exposure 10000 400`, `ae` | Manual exposure (microseconds, ISO) or auto |
| `wb 4500`, `awb` | White balance in kelvin, or auto |
| `ev 2`, `brightness 3`, `contrast -2`, `saturation 1`, `sharpness 2` | Image adjustments |
| `antibanding 50` | Light flicker: `50`, `60`, `off` or `auto` (by time zone) |
| `gain 20`, `mic off`, `mic on` | Microphone gain in dB, mute |
| `blur 24`, `blur 0` | Background blur strength (up to 64), or off |
| `bokeh lens`, `bokeh soft` | Lens-like or smooth blur |
| `background <picture>`, `background off` | A JPEG or PNG picture in place of the room |
| `lut noir 0.8`, `lut <file.cube>`, `lut off` | Colour look and its strength (`mono`, `noir`, `tonal`, `fade`, `chrome`, `instant`, `process`, `transfer`, `warm`, `cool`) |
| `warmth 30`, `tint`, `shadows`, `highlights`, `vibrance` | Lighting, -100 to 100 |
| `colour blue 0 -40 0` | One colour's hue, strength and lightness |
| `adjust reset` | Undo all lighting and colour changes |
| `zoom 0.5`, `zoom off`, `tracking slow` | Face tracking (0 loose to 1 tight) and how fast it follows |
| `magnify 1.5`, `magnify off` | Zoom into the middle, 1 to 4 |
| `pixelate 40`, `pixelate off` | Big square pixels |
| `touchup 50`, `tone 40 10`, `tone off` | Smoother skin; even skin tone and warmth |
| `gestures on`, `gesture pinch off` | Hand gestures, or one of them (`peace`, `pinch`) |
| `lockpause off` | Keep the camera running while the screen is locked |
| `loop record`, `loop stop`, `loop play`, `loop bounce`, `loop off` | Record up to 10 seconds and play it in place of live video |
| `preset save <name>`, `preset load <name>`, `preset list`, `preset delete <name>` | Presets |
| `sticker add ...`, `sticker list`, `sticker remove <n>`, `sticker clear` | Stickers, see below |

Stickers:
```shell
opal-rs ctl sticker add text 0.5 0.9 0.08 - white Back in 5 minutes
opal-rs ctl sticker add card 0.25 0.84 0.14 - Name | Title
opal-rs ctl sticker add clock 0.9 0.07 0.06 - white     # also timer, and picture <file> (JPEG or PNG)
opal-rs ctl sticker list                                # their numbers
opal-rs ctl sticker move 2 0.5 0.5 0.1                  # number, then where and how big
opal-rs ctl sticker flags 2 bf                          # number, then b, f, bf or -
```
The numbers are where the sticker's centre goes (across and down, 0 to 1) and its height (0 to 1). After them comes
`-`, or `b` to put the sticker behind the person, `f` to have it follow the face, or `bf`.

Where things are kept:

| | Linux | Windows |
| --- | --- | --- |
| Settings, presets, stickers | `~/.config/opal-rs` | `%APPDATA%\opal-rs` |
| Camera files | `~/.local/share/opal-rs/assets` | `%LOCALAPPDATA%\opal-rs\assets` |
| Service log | `journalctl --user -u opal-rs` | `%LOCALAPPDATA%\opal-rs\service.log` |

## Uninstalling
- **Linux:** `opal-rs uninstall` lists what it will remove and asks first. It also deletes settings and camera
  files (`--keep-data` keeps them; `--dry-run` only lists). `sudo apt purge opal-rs` alone keeps settings and
  camera files.
- **Windows:** Settings > Apps > Installed apps > Opal C1 (opal-rs) > Uninstall. It asks whether to delete
  settings and camera files too.

## Building it yourself
See [DEVELOPING.md](DEVELOPING.md) for building from source and building the packages.
