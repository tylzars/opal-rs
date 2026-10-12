# Installing

## Quick install
No Rust and no terminal setup: one package or installer, then the control panel asks for your own copy of the Opal
Composer disk image the first time it opens. It has a button that opens Opal's download page; download
`Opal_Composer_2.0.0_24.dmg` there and choose it. The camera files are copied out of it on your computer (Linux:
`~/.local/share/opal-rs/assets`, Windows: `%LOCALAPPDATA%\opal-rs\assets`).

The package and the installer are made as described in [Building the packages](#building-the-packages).

### Ubuntu or Debian (Ubuntu 24.04, Debian 13 or newer)
```shell
sudo apt install ./opal-rs_0.1.0-1_amd64.deb
```
The "Opal C1 Virtual" camera uses the kernel's `v4l2loopback` module, which Ubuntu 24.04's kernels include (6.8 and
the newer HWE kernels). If the install says this kernel has none, also install `v4l2loopback-dkms`; on a computer
with Secure Boot that one asks you to set a password and confirm it once after a restart (MOK enrollment). If the
install says v4l2loopback was already loaded with other settings, restart the computer.

Then log out and back in: the service and the tray icon start on their own. Open "Opal C1 Control" from the app menu
for the control panel. To remove it, see [Uninstalling](#uninstalling).

Other Linux distributions: build from source, below.

### Using the camera in several apps at once (Linux)
On Linux an app takes a camera for itself: while one app uses "Opal C1 Virtual", another cannot. Add a camera for
each extra app in the control panel (Virtual cameras), or:
```shell
opal-rs cameras                 # list them and which app uses each
opal-rs cameras add "Zoom"      # asks for your password
opal-rs cameras remove "Zoom"
opal-rs cameras apply           # make changes that waited for the cameras to be free
```
They all show the same video and effects; pick a different one in each app. Adding or removing one reloads
v4l2loopback, which only works while no app uses an Opal camera; otherwise the change waits for `opal-rs cameras
apply` or the next restart. There can be four extra cameras, named with letters, digits, spaces and `- _ . ( )`.
They are kept in `/etc/modprobe.d/opal-rs-v4l2loopback.conf`, which uninstalling removes. Windows shares a camera
between apps itself, so there is one camera there.

### Windows 11 (not yet tried on a real machine)
Run `opal-rs-setup.exe` and allow it to make changes. It installs into `C:\Program Files\opal-rs`, sets up the
"Opal C1 Virtual" camera, starts the tray icon when you sign in, adds "Opal C1 Control" to the Start menu and offers
to start it right away. To remove it, see [Uninstalling](#uninstalling).

## Uninstalling
```shell
opal-rs uninstall
```
It lists what it will remove and asks first, then stops the service, tray icon and control panel, deletes your
settings, presets, stickers and the camera files (`--keep-data` keeps them), and removes the service and autostart
files. With an administrator password prompt it removes the package, or the system files a source install copied in
(only exact copies of the ones from `dist/`). `--dry-run` only lists it all.

- `sudo apt purge opal-rs` alone removes the programs and system files (`apt remove` leaves the tray's autostart
  file) but keeps your settings and camera files in `~/.config/opal-rs` and `~/.local/share/opal-rs`. v4l2loopback stays loaded until you restart, and
  `sudo apt autoremove` removes `v4l2loopback-dkms` if nothing else uses it.
- On Windows, Settings > Apps > Installed apps > Opal C1 (opal-rs) > Uninstall does the same and asks whether to
  delete your settings and camera files too.
- The camera needs nothing: opal-rs never writes to its flash, and unplugging it returns it to its own firmware.

## Building from source on Linux

### Requirements
- Linux (Windows: see [Building on Windows](#building-on-windows))
- A Rust toolchain (`cargo`)
- Your own copy of the Opal Composer disk image listed in [README.md](README.md)

### Steps
1. Put the disk image at `assets/Opal_Composer_2.0.0_24.dmg` (see [assets/README.md](assets/README.md)).
2. Build and extract the camera files:
   ```shell
   cargo build --release
   target/release/opal-rs assets extract
   ```
   Each file is checked against its SHA-256. `opal-rs assets verify` checks them again.

### Running
1. Give yourself access to the camera and set up the virtual camera that apps will use. This needs the
   `v4l2loopback` kernel module (on Ubuntu or Debian: `sudo apt install v4l2loopback-dkms`).
   ```shell
   sudo cp dist/70-opal-c1.rules /etc/udev/rules.d/
   sudo udevadm control --reload
   sudo cp dist/v4l2loopback.conf /etc/modprobe.d/
   echo v4l2loopback | sudo tee /etc/modules-load.d/v4l2loopback.conf
   sudo modprobe v4l2loopback
   ```
   The first two commands let you use the camera without root. The rest create the "Opal C1 Virtual" camera now and
   every time the computer starts.
2. Install and start the service:
   ```shell
   cargo install --path crates/cli
   mkdir -p ~/.config/systemd/user
   cp dist/opal-rs.service ~/.config/systemd/user/
   systemctl --user enable --now opal-rs
   ```
   Apps see the camera as "Opal C1 Virtual" (`/dev/video10`). It streams while an app has it open and the LED is
   white while it streams.
3. Change settings while it runs:
   ```shell
   opal-rs ctl status
   opal-rs ctl focus 120      # manual focus, 0-255
   opal-rs ctl af             # continuous autofocus
   opal-rs ctl exposure 10000 400
   opal-rs ctl ae
   opal-rs ctl antibanding 50 # remove flicker from 50 Hz lights: 50, 60, off or auto (by time zone, the default)
   opal-rs ctl gain 20        # microphone gain in dB
   opal-rs ctl mic off        # mute the microphone; mic on turns it back on
   opal-rs ctl blur 24        # blur the background, radius in pixels of a 1080p picture; blur 0 turns it off
   opal-rs ctl bokeh lens     # lens look for the blur, bright lights become soft shapes; bokeh soft goes back
   opal-rs ctl background ~/Pictures/beach.jpg   # show a picture behind you instead; background off removes it
   opal-rs ctl lut noir 0.8   # colour look at 80%: mono, noir, tonal, fade, chrome, instant, process, transfer, warm or cool
   opal-rs ctl lut ~/looks/film.cube   # or a look from an Adobe .cube file; lut off removes it
   opal-rs ctl warmth 30      # warmer picture, -100 to 100; also tint, shadows, highlights and vibrance
   opal-rs ctl colour blue 0 -40 0   # one colour's hue, strength and lightness, -100 to 100 each
   opal-rs ctl adjust reset   # undo all of the above
   opal-rs ctl sticker add text 0.5 0.9 0.08 - white Back in 5 minutes
   opal-rs ctl sticker add card 0.25 0.84 0.14 - Your Name | Your title
   opal-rs ctl sticker add clock 0.9 0.07 0.06 - white   # also timer, and picture <file>
   opal-rs ctl loop record    # record up to 10 s; loop stop ends it, loop play (or bounce) shows it, loop off goes live
   opal-rs ctl preset save evening   # keep the current look, lighting and effects; preset load evening brings them back
   opal-rs ctl zoom 0.5       # keep your face framed, 0 loose to 1 tight; zoom off turns it off
   opal-rs ctl tracking slow  # how fast framing follows: slow, normal or fast
   opal-rs ctl magnify 1.5    # zoom into the middle, 1 to 4; magnify off goes back
   opal-rs ctl pixelate 40    # big square pixels, 1 to 100; pixelate off goes back
   opal-rs ctl touchup 50     # smoother skin, 0 to 100
   opal-rs ctl tone 40 10     # even out skin colour (0 to 100) and warm it (-100 to 100); tone off goes back
   opal-rs ctl gestures on    # hand gestures: a peace sign turns the video off and on, a pinch turns framing on and off
   opal-rs ctl gesture pinch off   # turn one gesture off (peace or pinch); gesture pinch on turns it back on
   opal-rs ctl resolution 2160p   # 720p, 1080p (default), 1440p or 2160p (4K); restarts the camera
   opal-rs ctl power low      # low power mode, see below; power normal goes back; restarts the camera
   ```
   `opal-rs ctl on` and `opal-rs ctl off` force streaming on or off; `opal-rs ctl auto` goes back to following apps.
   The camera also turns off while your screen is locked; `opal-rs ctl lockpause off` keeps it running.
   Focus, exposure, white balance, flicker, image adjustments, microphone gain and mute, blur, background, colour look, lighting and colour changes, zoom, pixelate, touch up, skin tone, framing,
   gestures, resolution and low power mode are saved in `~/.config/opal-rs/settings` and restored the next time the service starts.
   Stickers are placed by their centre, across and down as fractions of the picture, with their height as a
   fraction too. After the size comes `-`, or `b` to put the sticker behind you, `f` to have it follow your face
   (then across and down are around your face), or `bf`. `sticker list` shows them with their numbers;
   `sticker move <n> <across> <down> <size>`, `sticker flags <n> <b|f|bf|->`, `sticker remove <n>` and
   `sticker clear` change them. They are kept in `~/.config/opal-rs/stickers` and saved with presets.
   Presets are plain files in `~/.config/opal-rs/presets`; copy one there to import it. `preset list` and
   `preset delete <name>` manage them.

   In low power mode the camera sends 1080p video straight to your apps like an ordinary webcam, so your computer
   does less work, but there are no effects (blur, background, looks, framing, gestures). Pick "Opal C1" in your
   apps instead of "Opal C1 Virtual" while it is on.

4. Or use the control panel, which also shows a preview:
   ```shell
   cargo install --path crates/gui
   opal-rs-gui
   ```
5. For an icon in the top bar that shows when the camera is on and turns the video on and off:
   ```shell
   cargo install --path crates/tray
   mkdir -p ~/.config/autostart
   cp dist/opal-rs-tray.desktop ~/.config/autostart/
   opal-rs-tray &
   ```
   GNOME shows it through its AppIndicator extension, which Ubuntu turns on by default.

Starting the service reboots the camera, which drops it off USB for a few seconds. `opal-rs` refuses to boot it again
within a minute.

## Building on Windows
Windows 11 is needed: the virtual camera uses Windows 11's virtual camera support. Everything from Linux is there:
the "Opal C1 Virtual" camera with effects, `ctl`, the control panel and the tray icon. Low power mode streams for as
long as the service runs, as Windows does not say which apps use the camera's own video, and the control panel has
no preview then.

These are the steps `opal-rs-setup.exe` does for you, by hand:

1. Build the four files, from Linux with the MinGW linker (`sudo apt install gcc-mingw-w64-x86-64`) or on Windows:
   ```shell
   rustup target add x86_64-pc-windows-gnu
   cargo build --release --target x86_64-pc-windows-gnu -p opal-rs -p opal-gui -p opal-tray -p opal-vcam
   ```
   They are `opal-rs.exe`, `opal-rs-gui.exe`, `opal-rs-tray.exe` and `opal_vcam.dll` in
   `target/x86_64-pc-windows-gnu/release`.
2. Make a folder `C:\Program Files\opal-rs` and copy them there, with an `assets` folder holding the disk image. It
   has to be a folder like this one: Windows' camera service loads `opal_vcam.dll` and cannot read your user folders.
3. In that folder, in a PowerShell opened as administrator, register the virtual camera and extract the camera files:
   ```powershell
   regsvr32 opal_vcam.dll
   .\opal-rs assets extract
   ```
4. Start `opal-rs-tray.exe`. It starts the service, and "Opal C1 Virtual" appears in your apps. To have it start when
   you sign in, press Win+R, enter `shell:startup` and put a shortcut to `opal-rs-tray.exe` in the folder that opens.
5. `opal-rs ctl status` and the other `ctl` commands work as on Linux. Settings, presets and stickers are kept in
   `%APPDATA%\opal-rs`; the service writes its log to `%LOCALAPPDATA%\opal-rs\service.log`.

The camera asks Windows for the WinUSB driver on its data link (interface 0) in every state it boots through, so
Windows installs it on its own. Only if `opal-rs list` shows no camera, install WinUSB for interface 0 with
[Zadig](https://zadig.akeo.ie/) (Options > List All Devices), for each USB id the camera shows up under, leaving the
video and audio interfaces alone.

`opal-rs webcam` also works without the rest: the camera as an ordinary 1080p webcam until Ctrl-C.

To remove the virtual camera, run `regsvr32 /u opal_vcam.dll` as administrator.

## Building the packages
Both leave out the source tree's `assets/` folder (`OPAL_RS_PACKAGED=1`), so installed copies keep the camera files
in the user's data folder. Nothing from Opal goes into either.

The Linux package, with [cargo-deb](https://crates.io/crates/cargo-deb) (`cargo install cargo-deb`), written to
`target/debian/`:
```shell
OPAL_RS_PACKAGED=1 cargo build --release -p opal-rs -p opal-gui -p opal-tray
OPAL_RS_PACKAGED=1 cargo deb -p opal-rs --no-build
```
Its install script loads v4l2loopback, applies the USB rule and enables the service for every user.

The Windows installer, with the four programs built in (MinGW as above; `strip` keeps it smaller):
```shell
T=target/x86_64-pc-windows-gnu/release
OPAL_RS_PACKAGED=1 cargo build --release --target x86_64-pc-windows-gnu -p opal-rs -p opal-gui -p opal-tray -p opal-vcam
mkdir -p target/payload
for f in opal-rs.exe opal-rs-gui.exe opal-rs-tray.exe opal_vcam.dll; do x86_64-w64-mingw32-strip -o target/payload/$f $T/$f; done
OPAL_RS_PAYLOAD=$PWD/target/payload cargo build --release --target x86_64-pc-windows-gnu -p opal-setup --features payload
```
The installer is `$T/opal-rs-setup.exe`. Without `--features payload` it installs the four files from its own folder
instead.

## Tests
```shell
cargo test --release
```
With the disk image in `assets/`, this also extracts and verifies every file.
