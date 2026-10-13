# opal-rs

Use the Opal C1 webcam on Linux and Windows, with its effects, without Opal's own app.

opal-rs runs the camera and adds a camera called **"Opal C1 Virtual"** with blur, backgrounds, looks, face tracking
and the rest applied. A control panel and a tray icon change it all, even during a call.

## Getting started
1. Install the package for the system: see [INSTALL.md](INSTALL.md).
2. Open **Opal C1 Control**. The first time, it asks for Opal Composer, Opal's free app, which holds the camera's
   firmware and AI models.
3. In applications, choose **"Opal C1 Virtual"**.

Applications also list the camera's own "Opal C1", which gives no video while opal-rs runs the camera (except in low
power mode on Linux).

## Features
| | |
| --- | --- |
| Video | 720p, 1080p, 1440p or 4K. Focus, exposure, white balance and flicker control. |
| Background | Blur (soft or lens-like) or a picture in place of the room. |
| Look | Ten built-in colour looks or custom `.cube` files; lighting, colour and skin touch-up controls. |
| Framing | Face tracking keeps the face in the middle of the picture; digital zoom. |
| Stickers | Text, a name card, a clock, a timer or pictures, in front of or behind the person. |
| Gestures | A peace sign turns the video off and on; a pinch turns face tracking on and off. |
| Extras | Presets, a short video loop, pixelate, a camera-off picture, preview with zebra stripes and histogram. |
| Low power | A plain webcam with no effects, for slower computers (Linux). |

The camera only runs while an application uses it, and it stops while the screen is locked. Its light is white
while it sends video and blue while the video is off.

## Systems
| System | Support |
| --- | --- |
| Ubuntu 24.04, Debian 13 or newer | Everything, plus extra virtual cameras so several apps can use the C1 at once. |
| Windows 11 | Everything except low power mode. |
| Windows 10 | Not supported: Windows 10 has no virtual cameras. Opal C1 still works there as a plain webcam. |

## Requirements
The Opal Composer disk image, from Opal's download page:

| Release | File | SHA-256 |
| --- | --- | --- |
| Opal Composer 2.0 (build 24) | `Opal_Composer_2.0.0_24.dmg` | `4eaa1225a203b057dbabaa7b17d7bbff91512cac32e942359b651dbef06928b3` |

opal-rs does not include any of Opal's software; it takes what the camera needs from Opal Composer.

## More
- [INSTALL.md](INSTALL.md): installing, first run, everyday use, the command line, uninstalling.
- [DEVELOPING.md](DEVELOPING.md): building from source, the release packages, how the code fits together.
- [docs/c1-protocol.md](docs/c1-protocol.md): notes on the camera's USB interface.

Opal and Opal C1 are the property of Opal Camera Inc. This project is not affiliated with or endorsed by Opal Camera
Inc.

## License
Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
