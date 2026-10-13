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
    /// Booting is refused: the camera has stopped soon after starting on USB 2.
    #[error("the camera is connected over USB 2")]
    SlowUsb,
}

/// Boot progress. The `&str`s are the USB speed of the camera at that step.
pub enum Step {
    Rebooting(&'static str),
    Bootloader(Version, i32, &'static str),
    Sending(usize),
    Booted(&'static str),
}

/// Boots `firmware` with `config` and returns the device once it is running. A failed boot is not retried here:
/// every boot drops the camera off USB, so the caller spaces boots out. Nothing is sent to a running camera on USB 2.
pub fn boot(
    device: &Found,
    config: &BoardConfig,
    firmware: &[u8],
    mut on_step: impl FnMut(Step),
) -> Result<Found, Error> {
    if device.slow_usb() {
        return Err(Error::SlowUsb);
    }
    let serial = device.serial().unwrap_or_default().to_owned();
    let device = match device.state {
        // Booted is firmware left running when the last session stopped. Both are taken over as the camera's own
        // app does.
        State::FlashBooted | State::Booted => {
            on_step(Step::Rebooting(device.usb_speed()));
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
    on_step(Step::Bootloader(version, kind, device.usb_speed()));

    let image = boot_image(config, firmware);
    on_step(Step::Sending(image.len()));
    bl.boot_memory(&image)?;

    let booted =
        bootloader::wait_for(&serial, State::Booted, Duration::from_secs(10)).ok_or(Error::Missing(State::Booted))?;
    on_step(Step::Booted(booted.usb_speed()));
    Ok(booted)
}
