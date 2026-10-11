//! Board configuration and the boot image built from it.

use std::collections::BTreeMap;

use crate::nop;
use crate::{nop_enum, nop_struct};

const MAGIC1: u32 = 0x7801_0000;
const MAGIC2: u32 = 0x21ea_17e6;

/// Image header: magic, watchdog register writes, then a load record holding the board config.
const PREBOOT: [u8; 41] = [
    b'M', b'A', b'2', b'x', //
    0x9a, 0xa8, 0x00, 0x32, 0x20, 0xad, 0xde, 0xd0, 0xf1, //
    0x9a, 0x9c, 0x00, 0x32, 0x20, 0xff, 0xff, 0xff, 0xff, //
    0x9a, 0xa8, 0x00, 0x32, 0x20, 0xad, 0xde, 0xd0, 0xf1, //
    0x9a, 0xa4, 0x00, 0x32, 0x20, 0x01, 0x00, 0x00, 0x00, //
    0x8a,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum UsbSpeed {
    Unknown = 0,
    Low,
    Full,
    High,
    Super,
    SuperPlus,
}
nop_enum!(UsbSpeed as i32 { Unknown, Low, Full, High, Super, SuperPlus });

#[derive(Clone, Debug, PartialEq)]
pub struct Usb {
    pub vid: u16,
    pub pid: u16,
    pub flash_booted_vid: u16,
    pub flash_booted_pid: u16,
    pub max_speed: UsbSpeed,
}
nop_struct!(Usb {
    vid,
    pid,
    flash_booted_vid,
    flash_booted_pid,
    max_speed
});

impl Default for Usb {
    fn default() -> Self {
        Self {
            vid: 0x03e7,
            pid: 0xf63b,
            flash_booted_vid: 0x03e7,
            flash_booted_pid: 0xf63d,
            max_speed: UsbSpeed::Super,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Network {
    pub mtu: u16,
}
nop_struct!(Network { mtu });

#[derive(Clone, Debug, PartialEq)]
pub struct Gpio {
    pub mode: i8,
    pub direction: i8,
    pub level: i8,
    pub pull: i8,
    pub drive: i8,
    pub schmitt: bool,
    pub slew_fast: bool,
}
nop_struct!(Gpio {
    mode,
    direction,
    level,
    pull,
    drive,
    schmitt,
    slew_fast
});

#[derive(Clone, Debug, PartialEq)]
pub struct Uart {
    pub tmp: i8,
}
nop_struct!(Uart { tmp });

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoardConfig {
    pub usb: Usb,
    pub network: Network,
    pub sysctl: Vec<String>,
    pub watchdog_timeout_ms: Option<u32>,
    pub watchdog_initial_delay_ms: Option<u32>,
    pub gpio: BTreeMap<i8, Gpio>,
    pub uart: BTreeMap<i8, Uart>,
    pub uvc_enable: Option<bool>,
}
nop_struct!(BoardConfig {
    usb,
    network,
    sysctl,
    watchdog_timeout_ms,
    watchdog_initial_delay_ms,
    gpio,
    uart,
    uvc_enable
});

fn djb2(data: &[u8]) -> u32 {
    data.iter()
        .fold(5381u32, |h, &b| h.wrapping_mul(33).wrapping_add(b as u32))
}

/// Prepends the board configuration to an application image.
pub fn boot_image(config: &BoardConfig, firmware: &[u8]) -> Vec<u8> {
    let payload = nop::to_vec(config);
    let body = payload.len() + 12;
    let pad = (4 - body % 4) % 4;
    let mut out = Vec::with_capacity(PREBOOT.len() + 6 + body + pad + firmware.len());
    out.extend_from_slice(&PREBOOT);
    out.extend_from_slice(&MAGIC1.to_le_bytes());
    out.extend_from_slice(&(((body + pad) / 4) as u16).to_le_bytes());
    out.extend_from_slice(&MAGIC2.to_le_bytes());
    out.extend_from_slice(&djb2(&payload).to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    out.resize(out.len() + pad, 0);
    out.extend_from_slice(firmware);
    out
}

impl BoardConfig {
    pub fn encode(&self) -> Vec<u8> {
        nop::to_vec(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_bytes() {
        let b = BoardConfig::default().encode();
        assert_eq!(
            b,
            [
                0xb9, 8, // BoardConfig, 8 members
                0xb9, 5, 0x81, 0xe7, 0x03, 0x81, 0x3b, 0xf6, 0x81, 0xe7, 0x03, 0x81, 0x3d, 0xf6, 4, // usb
                0xb9, 1, 0, // network
                0xba, 0, // sysctl
                0xbe, 0xbe, // watchdog options
                0xbb, 0, 0xbb, 0,    // gpio, uart
                0xbe, // uvcEnable
            ]
        );
    }

    #[test]
    fn image_header_layout() {
        let img = boot_image(&BoardConfig::default(), &[0xaa]);
        assert_eq!(&img[..4], b"MA2x");
        assert_eq!(img[40], 0x8a);
        assert_eq!(&img[41..45], &MAGIC1.to_le_bytes());
        let words = u16::from_le_bytes([img[45], img[46]]) as usize;
        assert_eq!(&img[47..51], &MAGIC2.to_le_bytes());
        assert_eq!(img.len(), 47 + words * 4 + 1);
        assert_eq!(*img.last().unwrap(), 0xaa);
    }

    #[test]
    fn djb2_reference() {
        assert_eq!(djb2(b""), 5381);
        assert_eq!(djb2(b"a"), 5381 * 33 + 97);
    }
}
