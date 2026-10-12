# Installing

## Requirements
- Linux
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
   opal-rs ctl blur 24        # blur the background, radius in pixels; blur 0 turns it off
   opal-rs ctl background ~/Pictures/beach.jpg   # show a picture behind you instead; background off removes it
   opal-rs ctl lut noir 0.8   # colour look at 80%: mono, noir, tonal, fade, chrome, instant, process, transfer, warm or cool
   opal-rs ctl lut ~/looks/film.cube   # or a look from an Adobe .cube file; lut off removes it
   opal-rs ctl zoom 0.5       # keep your face framed, 0 loose to 1 tight; zoom off turns it off
   opal-rs ctl tracking slow  # how fast framing follows: slow, normal or fast
   opal-rs ctl gestures on    # hand gestures: a peace sign turns the video off and on, a pinch turns framing on and off
   opal-rs ctl resolution 1440p   # 720p, 1080p (default) or 1440p; restarts the camera
   opal-rs ctl power low      # low power mode, see below; power normal goes back; restarts the camera
   ```
   `opal-rs ctl on` and `opal-rs ctl off` force streaming on or off; `opal-rs ctl auto` goes back to following apps.
   Focus, exposure, white balance, flicker, image adjustments, microphone gain and mute, blur, background, colour look, framing,
   gestures, resolution and low power mode are saved in `~/.config/opal-rs/settings` and restored the next time the service starts.

   In low power mode the camera sends 1080p video straight to your apps like an ordinary webcam, so your computer
   does less work, but there are no effects (blur, background, looks, framing, gestures). Pick "Opal C1" in your
   apps instead of "Opal C1 Virtual" while it is on.

4. Or use the control panel, which also shows a preview:
   ```shell
   cargo install --path crates/gui
   opal-rs-gui
   ```

Starting the service reboots the camera, which drops it off USB for a few seconds. `opal-rs` refuses to boot it again
within a minute.

## Tests
```shell
cargo test --release
```
With the disk image in `assets/`, this also extracts and verifies every file.
