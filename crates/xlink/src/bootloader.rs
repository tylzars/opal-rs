//! Myriad X bootloader client: version queries and booting an image from RAM.
//!
//! Requests that write flash or bootloader configuration are deliberately not implemented.

use std::thread;
use std::time::Duration;

use nusb::MaybeFuture;
use nusb::transfer::{ControlOut, ControlType, Recipient};

use crate::link::{Error, Link, StreamId};

const STREAM: &str = "__bootloader";
const STREAM_SIZE: u32 = 5 << 20;

const GET_BOOTLOADER_VERSION: u32 = 3;
const BOOT_MEMORY: u32 = 4;
const GET_BOOTLOADER_TYPE: u32 = 8;

const RESP_BOOTLOADER_VERSION: u32 = 2;
const RESP_BOOTLOADER_TYPE: u32 = 3;

/// Asks an application started from flash to reboot into the bootloader.
pub fn reboot_to_bootloader(info: &nusb::DeviceInfo) -> Result<(), Error> {
    let device = info.open().wait().map_err(std::io::Error::from)?;
    let req = ControlOut {
        control_type: ControlType::Standard,
        recipient: Recipient::Device,
        request: 0xf5,
        value: 0x0da1,
        index: 0,
        data: &[],
    };
    // The device usually drops off the bus before acknowledging.
    #[cfg(not(windows))]
    let _ = device.control_out(req, Duration::from_secs(1)).wait();
    // WinUSB only sends control requests through a claimed interface.
    #[cfg(windows)]
    if let Ok(iface) = device.claim_interface(0).wait() {
        let _ = iface.control_out(req, Duration::from_secs(1)).wait();
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

pub struct Bootloader {
    link: Link,
    stream: StreamId,
}

impl Bootloader {
    pub fn connect(info: &nusb::DeviceInfo) -> Result<Self, Error> {
        let link = Link::connect(info, Duration::from_secs(5))?;
        let stream = link.open_stream(STREAM, STREAM_SIZE)?;
        Ok(Self { link, stream })
    }

    fn call(&self, cmd: u32, expect: u32, min_len: usize) -> Result<Vec<u8>, Error> {
        self.link.write(self.stream, &cmd.to_le_bytes())?;
        let r = self.link.read(STREAM, Some(Duration::from_secs(5)))?;
        if r.len() < min_len || u32::from_le_bytes(r[0..4].try_into().unwrap()) != expect {
            return Err(Error::Refused("unexpected bootloader response"));
        }
        Ok(r)
    }

    pub fn version(&self) -> Result<Version, Error> {
        let r = self.call(GET_BOOTLOADER_VERSION, RESP_BOOTLOADER_VERSION, 16)?;
        let u = |o: usize| u32::from_le_bytes(r[o..o + 4].try_into().unwrap());
        let v = Version(u(4), u(8), u(12));
        // Untagged early bootloaders report 0.0.0.
        Ok(if v == Version(0, 0, 0) { Version(0, 0, 4) } else { v })
    }

    /// 0 = USB, 1 = network. Needs bootloader 0.0.12 or newer.
    pub fn kind(&self) -> Result<i32, Error> {
        let r = self.call(GET_BOOTLOADER_TYPE, RESP_BOOTLOADER_TYPE, 8)?;
        Ok(i32::from_le_bytes(r[4..8].try_into().unwrap()))
    }

    /// Sends an application image to RAM and starts it. The link drops when it boots.
    pub fn boot_memory(self, image: &[u8]) -> Result<(), Error> {
        let packets = image.len().div_ceil(STREAM_SIZE as usize) as u32;
        let mut req = Vec::with_capacity(12);
        req.extend_from_slice(&BOOT_MEMORY.to_le_bytes());
        req.extend_from_slice(&(image.len() as u32).to_le_bytes());
        req.extend_from_slice(&packets.to_le_bytes());
        self.link.write(self.stream, &req)?;
        self.link.write_split(self.stream, image, STREAM_SIZE as usize)?;
        self.link.wait_closed(Duration::from_secs(10));
        Ok(())
    }
}

/// Waits for a device in `state` to appear, preferring one with the given serial.
pub fn wait_for(serial: &str, state: crate::State, timeout: Duration) -> Option<crate::Found> {
    let step = Duration::from_millis(100);
    let mut waited = Duration::ZERO;
    while waited < timeout {
        if let Ok(list) = crate::list() {
            let mut matching: Vec<_> = list.into_iter().filter(|f| f.state == state).collect();
            if let Some(i) = matching.iter().position(|f| f.serial() == Some(serial)) {
                return Some(matching.swap_remove(i));
            }
            if matching.len() == 1 {
                return matching.pop();
            }
        }
        thread::sleep(step);
        waited += step;
    }
    None
}
