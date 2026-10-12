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
   opal-rs ctl gain 20        # microphone gain in dB
   opal-rs ctl blur 24        # blur the background, radius in pixels; blur 0 turns it off
   opal-rs ctl lut ~/looks/warm.cube 0.8   # colour look from an Adobe .cube file, at 80%; lut off removes it
   opal-rs ctl zoom 0.5       # keep your face framed, 0 loose to 1 tight; zoom off turns it off
   opal-rs ctl tracking slow  # how fast framing follows: slow, normal or fast
   opal-rs ctl resolution 1440p   # 720p, 1080p (default) or 1440p; restarts the camera
   ```
   `opal-rs ctl on` and `opal-rs ctl off` force streaming on or off; `opal-rs ctl auto` goes back to following apps.
   Focus, exposure, white balance, image adjustments, microphone gain, blur, colour look and framing are saved in
   `~/.config/opal-rs/settings` and restored the next time the service starts.

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
