# Installing

## Requirements
- Linux
- A Rust toolchain (`cargo`)
- Your own copy of the Opal Composer disk image listed in [README.md](README.md)

## Steps
1. Build:
   ```shell
   cargo build --release
   ```
2. Extract the camera files:
   ```shell
   target/release/opal-rs assets extract /path/to/Opal_Composer_2.0.0_24.dmg
   ```
   Files go to `~/.local/share/opal-rs/assets`, or to `--dir`. Each file is checked against its SHA-256;
   `opal-rs assets verify` checks them again.

## Tests
```shell
cargo test
OPAL_COMPOSER_DMG=/path/to/Opal_Composer_2.0.0_24.dmg cargo test --release
```
