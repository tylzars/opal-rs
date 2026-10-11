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

## Tests
```shell
cargo test --release
```
With the disk image in `assets/`, this also extracts and verifies every file.
