//! The Windows virtual camera "Opal C1 Virtual".
//!
//! Windows' Frame Server loads this crate's DLL as a Media Foundation media source when an app opens the camera.
//! The source creates a shared frame buffer ([`shared`]) and marks it while an app streams; the opal-rs service
//! ([`host`]) registers the camera for its session, watches that mark and writes the processed frames in.

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
