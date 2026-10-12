//! Installs opal-rs on Windows: the programs and the virtual camera in Program Files, the tray at sign-in, a Start
//! menu entry for the control panel and an entry in Apps & features. `opal-rs-setup uninstall` removes them again.
//! The camera files are not part of it; the control panel asks for the user's own Opal Composer disk image.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod install;

#[cfg(windows)]
fn main() {
    install::main();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("opal-rs-setup installs opal-rs on Windows; on Linux see INSTALL.md");
    std::process::exit(1);
}
