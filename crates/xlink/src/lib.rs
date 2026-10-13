//! XLink transport for the Opal C1.

pub mod bootloader;
pub mod link;
mod usb;

pub use link::{Error, Link, StreamId};

/// Locking that carries on after another thread panicked while holding the lock, instead of panicking too.
pub trait LockExt<T> {
    fn locked(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> LockExt<T> for std::sync::Mutex<T> {
    fn locked(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

use nusb::MaybeFuture;

pub const VID: u16 = 0x03e7;

/// What the Myriad X is running, from its USB product ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// USB ROM boot, nothing loaded
    Unbooted,
    /// Bootloader waiting for an application
    Bootloader,
    /// Application started by a host
    Booted,
    /// Application started from flash
    FlashBooted,
}

impl State {
    pub fn from_pid(pid: u16) -> Option<Self> {
        match pid {
            0x2485 => Some(Self::Unbooted),
            0xf63c => Some(Self::Bootloader),
            0xf63b => Some(Self::Booted),
            0xf63d => Some(Self::FlashBooted),
            _ => None,
        }
    }

    pub fn pid(self) -> u16 {
        match self {
            Self::Unbooted => 0x2485,
            Self::Bootloader => 0xf63c,
            Self::Booted => 0xf63b,
            Self::FlashBooted => 0xf63d,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Found {
    pub info: nusb::DeviceInfo,
    pub state: State,
}

impl Found {
    pub fn serial(&self) -> Option<&str> {
        self.info.serial_number()
    }

    /// Whether a running camera is on a USB 2 or slower connection; `false` when the system does not say. The
    /// bootloader always runs at USB 2 speed, so it never counts.
    pub fn slow_usb(&self) -> bool {
        self.state != State::Bootloader
            && matches!(
                self.info.speed(),
                Some(nusb::Speed::Low | nusb::Speed::Full | nusb::Speed::High)
            )
    }

    /// The connection's speed as the system reports it, in the camera's own words (`high` is USB 2).
    pub fn usb_speed(&self) -> &'static str {
        match self.info.speed() {
            Some(nusb::Speed::Low) => "low",
            Some(nusb::Speed::Full) => "full",
            Some(nusb::Speed::High) => "high",
            Some(nusb::Speed::Super) => "super",
            Some(nusb::Speed::SuperPlus) => "super-plus",
            _ => "unknown",
        }
    }
}

/// Myriad X devices currently attached. Reads the OS device list only.
pub fn list() -> Result<Vec<Found>, nusb::Error> {
    Ok(nusb::list_devices()
        .wait()?
        .filter(|d| d.vendor_id() == VID)
        .filter_map(|info| State::from_pid(info.product_id()).map(|state| Found { info, state }))
        .collect())
}
