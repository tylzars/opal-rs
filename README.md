# opal-rs

Linux host software for the Opal C1 webcam, written in Rust.

**No Opal software is included.** The repository holds no firmware, neural networks, tuning data or code from Opal
Composer. The files the camera needs are extracted from your own copy of the Opal Composer disk image; see
[INSTALL.md](INSTALL.md).

| Release | Disk image | SHA-256 |
| --- | --- | --- |
| Opal Composer 2.0 (build 24) | `Opal_Composer_2.0.0_24.dmg` | `4eaa1225a203b057dbabaa7b17d7bbff91512cac32e942359b651dbef06928b3` |

## What it does
- Boots the camera's firmware from RAM and runs it as a webcam: video goes to a virtual camera
  ("Opal C1 Virtual", via `v4l2loopback`), the microphone appears as a normal USB microphone.
- Streams only while an app uses the camera; the status LED is white while it streams.
- Focus, exposure, white balance, light flicker (50/60 Hz), image adjustments, microphone gain and mute.
- Background blur or a background picture, using person segmentation running on the camera.
- Colour looks: ten built in (black and white, faded, vivid, warm, cool and more), or your own Adobe `.cube` files.
- Face framing: the picture follows your face, using face detection running on the camera.
- Hand gestures: hold up a peace sign to turn the video off and on, or pinch to turn face framing on and off.
- Low power mode: the camera works as an ordinary webcam with no effects, so your computer does less work.
- On a USB 2 port the video drops to 720p, as the official app does.
- Runs as a systemd user service controlled with `opal-rs ctl`; settings are kept between runs.

It never writes to the camera's flash. Unplugging the camera returns it to its built-in firmware.

## Documentation
- [INSTALL.md](INSTALL.md): building and extracting the camera files
- [docs/c1-protocol.md](docs/c1-protocol.md): notes on the camera's USB interface

Opal and Opal C1 are the property of Opal Camera Inc. This project is not affiliated with or endorsed by Opal Camera
Inc.

## License
Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
