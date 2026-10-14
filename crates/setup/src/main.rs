//! Windows installer and uninstaller (`opal-rs-setup uninstall`). Holds no Opal files.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod accounts;
#[cfg(windows)]
mod camera;
#[cfg(windows)]
mod install;
#[cfg_attr(not(windows), allow(dead_code))]
mod pending;

/// Windows reads this when loading the program: `dependent_load_flags` 0x800 (LOAD_LIBRARY_SEARCH_SYSTEM32) has the
/// system DLLs setup imports come only from System32, never from the folder setup was downloaded to, as setup runs
/// as administrator. The MinGW linker does not point the program header at it; `cargo xtask exe` does, finding it by
/// [`LOAD_CONFIG_MARK`] in its time stamp field.
#[cfg(all(windows, target_env = "gnu"))]
#[repr(C)]
struct LoadConfig {
    size: u32,
    time_date_stamp: u32,
    major_version: u16,
    minor_version: u16,
    global_flags_clear: u32,
    global_flags_set: u32,
    critical_section_default_timeout: u32,
    de_commit_free_block_threshold: u64,
    de_commit_total_free_threshold: u64,
    lock_prefix_table: u64,
    maximum_allocation_size: u64,
    virtual_memory_threshold: u64,
    process_affinity_mask: u64,
    process_heap_flags: u32,
    csd_version: u16,
    dependent_load_flags: u16,
    edit_list: u64,
    security_cookie: u64,
    /// The newer fields, all unused, so the structure has the size Windows expects of a 64-bit program.
    rest: [u8; 0x140 - 96],
}

#[cfg(all(windows, target_env = "gnu"))]
#[unsafe(no_mangle)]
#[used]
static _load_config_used: LoadConfig = LoadConfig {
    size: size_of::<LoadConfig>() as u32,
    time_date_stamp: LOAD_CONFIG_MARK,
    major_version: 0,
    minor_version: 0,
    global_flags_clear: 0,
    global_flags_set: 0,
    critical_section_default_timeout: 0,
    de_commit_free_block_threshold: 0,
    de_commit_total_free_threshold: 0,
    lock_prefix_table: 0,
    maximum_allocation_size: 0,
    virtual_memory_threshold: 0,
    process_affinity_mask: 0,
    process_heap_flags: 0,
    csd_version: 0,
    dependent_load_flags: 0x800,
    edit_list: 0,
    security_cookie: 0,
    rest: [0; 0x140 - 96],
};

/// "OPAL", little-endian.
#[cfg(all(windows, target_env = "gnu"))]
const LOAD_CONFIG_MARK: u32 = 0x4c41_504f;

#[cfg(windows)]
fn main() {
    install::main();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("opal-rs-setup installs opal-rs on Windows; on Linux see INSTALL.md");
    std::process::exit(1);
}
