# opal-rs

Linux host software for the Opal C1 webcam, written in Rust.

**No Opal software is included.** The repository holds no firmware, neural networks, tuning data or code from Opal
Composer. The files the camera needs are extracted from your own copy of the Opal Composer disk image; see
[INSTALL.md](INSTALL.md).

| Release | Disk image | SHA-256 |
| --- | --- | --- |
| Opal Composer 2.0 (build 24) | `Opal_Composer_2.0.0_24.dmg` | `4eaa1225a203b057dbabaa7b17d7bbff91512cac32e942359b651dbef06928b3` |

## Documentation
- [INSTALL.md](INSTALL.md): building and extracting the camera files
- [docs/c1-protocol.md](docs/c1-protocol.md): notes on the camera's USB interface

Opal and Opal C1 are the property of Opal Camera Inc. This project is not affiliated with or endorsed by Opal Camera
Inc.

## License
Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
