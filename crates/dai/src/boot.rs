//! Starting an application image on the camera from RAM.

use std::time::Duration;

use opal_xlink::bootloader::{self, Bootloader, Version};
use opal_xlink::{Found, State};

use crate::board::{BoardConfig, boot_image};

/// `BootMemory` needs this bootloader version or newer.
const MIN_BOOTLOADER: Version = Version(0, 0, 12);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Link(#[from] opal_xlink::Error),
    #[error("no {0:?} device appeared")]
    Missing(State),
    #[error("bootloader {0} is too old to boot from RAM")]
    OldBootloader(Version),
}

pub enum Step {
    Rebooting,
    Bootloader(Version, i32),
    Sending(usize),
    Booted,
}

/// Boots `firmware` with `config` and returns the device once it is running.
///
/// If the bootloader drops away mid-boot (it times out and starts the application in flash), the camera is taken
/// again once, so it is not left cycling between the two.
pub fn boot(
    device: &Found,
    config: &BoardConfig,
    firmware: &[u8],
    mut on_step: impl FnMut(Step),
) -> Result<Found, Error> {
    let first = match boot_once(device, config, firmware, &mut on_step) {
        Ok(f) => return Ok(f),
        Err(e) => e,
    };
    let serial = device.serial().unwrap_or_default().to_owned();
    let again = bootloader::wait_for(&serial, State::Bootloader, Duration::from_secs(15))
        .or_else(|| bootloader::wait_for(&serial, State::FlashBooted, Duration::from_secs(1)))
        .ok_or(first)?;
    boot_once(&again, config, firmware, &mut on_step)
}

fn boot_once(
    device: &Found,
    config: &BoardConfig,
    firmware: &[u8],
    on_step: &mut impl FnMut(Step),
) -> Result<Found, Error> {
    let serial = device.serial().unwrap_or_default().to_owned();
    let device = match device.state {
        // Booted is an application left running by an earlier session.
        State::FlashBooted | State::Booted => {
            on_step(Step::Rebooting);
            bootloader::reboot_to_bootloader(&device.info)?;
            bootloader::wait_for(&serial, State::Bootloader, Duration::from_secs(5))
                .ok_or(Error::Missing(State::Bootloader))?
        }
        State::Bootloader => device.clone(),
        State::Unbooted => return Err(Error::Missing(State::Bootloader)),
    };

    // Right after the bootloader appears, udev may not have given us access yet.
    let started = std::time::Instant::now();
    let bl = loop {
        match Bootloader::connect(&device.info) {
            Ok(bl) => break bl,
            Err(_) if started.elapsed() < Duration::from_secs(3) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e.into()),
        }
    };
    let version = bl.version()?;
    if version < MIN_BOOTLOADER {
        drop(bl);
        return Err(Error::OldBootloader(version));
    }
    let kind = bl.kind()?;
    on_step(Step::Bootloader(version, kind));

    let image = boot_image(config, firmware);
    on_step(Step::Sending(image.len()));
    bl.boot_memory(&image)?;

    let booted =
        bootloader::wait_for(&serial, State::Booted, Duration::from_secs(10)).ok_or(Error::Missing(State::Booted))?;
    on_step(Step::Booted);
    Ok(booted)
}
