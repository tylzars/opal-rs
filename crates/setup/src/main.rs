//! Windows installer and uninstaller (`opal-rs-setup uninstall`). Holds no Opal files.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod install;
#[cfg_attr(not(windows), allow(dead_code))]
mod pending;

#[cfg(windows)]
fn main() {
    install::main();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("opal-rs-setup installs opal-rs on Windows; on Linux see INSTALL.md");
    std::process::exit(1);
}
