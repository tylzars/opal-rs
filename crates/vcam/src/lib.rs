//! The Windows virtual camera "Opal C1 Virtual": a Media Foundation source fed by the service.

pub mod shared;

#[cfg(windows)]
mod dll;
#[cfg(windows)]
pub mod host;
#[cfg(windows)]
mod source;

/// Class id of the media source, as registered by `regsvr32 opal_vcam.dll`.
pub const CLSID: u128 = 0x4e40c079_e97c_4deb_84f6_42aa485dc655;

/// The name apps show.
pub const NAME: &str = "Opal C1 Virtual";
