# Installing

## Requirements
- Linux (Windows: see [Windows](#windows-experimental))
- A Rust toolchain (`cargo`)
- Your own copy of the Opal Composer disk image listed in [README.md](README.md)

## Steps
1. Put the disk image at `assets/Opal_Composer_2.0.0_24.dmg` (see [assets/README.md](assets/README.md)).
2. Build and extract the camera files:
   ```shell
   cargo build --release
   target/release/opal-rs assets extract
   ```
   Each file is checked against its SHA-256. `opal-rs assets verify` checks them again.

## Running
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

## Windows (experimental)
On Windows only `opal-rs webcam` is available: it boots the camera in low power mode and keeps its own 1080p video on
until Ctrl-C, so apps see an ordinary webcam with autofocus and auto exposure. There are no effects, no control
panel and no service yet. `list`, `info` and `assets` work too.

1. Build `opal-rs.exe`, on Windows with `cargo build --release -p opal-rs`, or from Linux with the MinGW linker
   (`sudo apt install gcc-mingw-w64-x86-64`):
   ```shell
   rustup target add x86_64-pc-windows-gnu
   cargo build --release -p opal-rs --target x86_64-pc-windows-gnu
   ```
2. Put `opal-rs.exe` in a folder with an `assets` folder next to it, put the disk image in `assets`, and run
   `opal-rs assets extract`. (Or point `OPAL_RS_ASSETS` at a folder holding the camera files.)
3. The camera asks Windows for the WinUSB driver on its data link (interface 0) in every state it boots through, so
   Windows 8 and later install it on their own. Only if `opal-rs list` shows no camera, install WinUSB for interface
   0 with [Zadig](https://zadig.akeo.ie/) (Options > List All Devices), for each USB id the camera shows up under.
   Leave the video and audio interfaces alone.
4. Run `opal-rs webcam` and pick "Opal C1" in your app.

## Tests
```shell
cargo test --release
```
With the disk image in `assets/`, this also extracts and verifies every file.
