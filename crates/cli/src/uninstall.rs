//! `opal-rs uninstall`: removes opal-rs and what it made, after listing it all and asking.
//!
//! The camera itself needs nothing: opal-rs never writes to its flash, and unplugging it returns it to its own
//! firmware.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::Result;

/// What uninstalling does, in order.
#[derive(Debug, PartialEq)]
enum Step {
    /// Stops the service, the tray and the control panel.
    Stop,
    /// Deletes a file or folder of the user's.
    Remove(PathBuf),
    /// Runs a command as administrator (Linux; on Windows the installer asks for that itself).
    #[cfg_attr(windows, allow(dead_code))]
    Admin(Vec<String>),
    /// Runs a command as the user.
    Run(Vec<String>),
}

impl Step {
    fn describe(&self) -> String {
        match self {
            Step::Stop => "stop the opal-rs service, tray icon and control panel".into(),
            Step::Remove(p) => format!("delete {}", p.display()),
            Step::Admin(c) => format!("as administrator: {}", c.join(" ")),
            Step::Run(c) => format!("run: {}", c.join(" ")),
        }
    }
}

#[cfg(unix)]
fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// System files a source install copies in (INSTALL.md), with what they hold; only exact copies are removed.
#[cfg(unix)]
const SYSTEM_FILES: [(&str, &str); 3] = [
    (
        "/etc/udev/rules.d/70-opal-c1.rules",
        include_str!("../../../dist/70-opal-c1.rules"),
    ),
    (
        "/etc/modprobe.d/v4l2loopback.conf",
        include_str!("../../../dist/v4l2loopback.conf"),
    ),
    ("/etc/modules-load.d/v4l2loopback.conf", "v4l2loopback\n"),
];

/// The steps for this computer. `keep_data` keeps settings and camera files.
fn plan(keep_data: bool) -> Vec<Step> {
    let mut steps = vec![Step::Stop];
    if !keep_data {
        for dir in [crate::platform::config_dir(), crate::platform::data_dir()] {
            if dir.exists() {
                steps.push(Step::Remove(dir));
            }
        }
    }
    #[cfg(unix)]
    {
        let user_files = [
            home().join(".config/systemd/user/opal-rs.service"),
            home().join(".config/autostart/opal-rs-tray.desktop"),
        ];
        let had_unit = user_files[0].exists();
        steps.extend(user_files.into_iter().filter(|p| p.exists()).map(Step::Remove));
        if had_unit {
            steps.push(Step::Run(vec![
                "systemctl".into(),
                "--user".into(),
                "daemon-reload".into(),
            ]));
        }
        let packaged = Command::new("dpkg-query")
            .args(["-W", "-f=${Status}", "opal-rs"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("install ok installed"));
        if packaged {
            // The package takes its programs and system files with it.
            steps.push(Step::Admin(
                ["apt-get", "remove", "-y", "opal-rs"].map(String::from).into(),
            ));
        }
        let ours: Vec<String> = SYSTEM_FILES
            .iter()
            .filter(|(path, text)| std::fs::read_to_string(path).is_ok_and(|t| t.trim() == text.trim()))
            .map(|(path, _)| path.to_string())
            .collect();
        if !ours.is_empty() {
            let mut rm = vec!["rm".to_owned(), "-f".to_owned()];
            rm.extend(ours);
            steps.push(Step::Admin(rm));
            steps.push(Step::Admin(
                ["udevadm", "control", "--reload-rules"].map(String::from).into(),
            ));
        }
        // Programs installed with `cargo install`.
        let cargo_bin = std::env::var_os("CARGO_HOME")
            .map_or_else(|| home().join(".cargo"), PathBuf::from)
            .join("bin");
        let crates: Vec<&str> = [
            ("opal-rs", "opal-rs"),
            ("opal-gui", "opal-rs-gui"),
            ("opal-tray", "opal-rs-tray"),
        ]
        .into_iter()
        .filter(|(_, bin)| cargo_bin.join(bin).exists())
        .map(|(krate, _)| krate)
        .collect();
        if !crates.is_empty() {
            let mut c = vec!["cargo".to_owned(), "uninstall".to_owned()];
            c.extend(crates.into_iter().map(String::from));
            steps.push(Step::Run(c));
        }
    }
    #[cfg(windows)]
    {
        let setup = std::env::var_os("ProgramFiles")
            .map_or_else(|| PathBuf::from("C:\\Program Files"), PathBuf::from)
            .join("opal-rs\\opal-rs-setup.exe");
        if setup.exists() {
            steps.push(Step::Run(vec![setup.display().to_string(), "uninstall".into()]));
        }
    }
    steps
}

fn stop() {
    let _ = opal_ipc::request("quit", Duration::from_secs(3));
    for _ in 0..30 {
        if opal_ipc::request("status", Duration::from_secs(1)).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    #[cfg(unix)]
    {
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", "opal-rs.service"])
            .output();
        for name in ["opal-rs-tray", "opal-rs-gui"] {
            let _ = Command::new("pkill").args(["-x", name]).output();
        }
    }
    #[cfg(windows)]
    for name in ["opal-rs-tray.exe", "opal-rs-gui.exe"] {
        let mut c = Command::new("taskkill");
        c.args(["/F", "/IM", name]);
        std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000);
        let _ = c.output();
    }
}

fn remove(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

fn run(command: &[String], admin: bool) -> std::io::Result<bool> {
    let mut c = if admin && cfg!(unix) {
        // sudo asks in the terminal; without one, pkexec asks through the desktop.
        use std::io::IsTerminal;
        let mut c = Command::new(if std::io::stdin().is_terminal() { "sudo" } else { "pkexec" });
        c.args(command);
        c
    } else {
        let mut c = Command::new(&command[0]);
        c.args(&command[1..]);
        c
    };
    Ok(c.status()?.success())
}

/// Does each step, going on after a failure. True if all went well.
fn carry_out(steps: &[Step]) -> bool {
    let mut ok = true;
    for s in steps {
        let result = match s {
            Step::Stop => {
                stop();
                Ok(true)
            }
            Step::Remove(p) => remove(p).map(|_| true),
            Step::Admin(c) => run(c, true),
            Step::Run(c) => run(c, false),
        };
        match result {
            Ok(true) => {}
            Ok(false) => {
                ok = false;
                eprintln!("failed: {}", s.describe());
            }
            Err(e) => {
                ok = false;
                eprintln!("failed: {}: {e}", s.describe());
            }
        }
    }
    ok
}

/// Lists the steps, asks unless `yes`, and carries them out. `dry_run` only lists them.
pub fn uninstall(keep_data: bool, yes: bool, dry_run: bool) -> Result<bool> {
    let steps = plan(keep_data);
    println!("This will:");
    for s in &steps {
        println!("  - {}", s.describe());
    }
    if keep_data {
        println!("Your settings and camera files are kept.");
    }
    println!("The camera needs nothing: unplugging it returns it to its own firmware.");
    if dry_run {
        return Ok(true);
    }
    if !yes {
        print!("Go ahead? [y/N] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("Nothing was changed.");
            return Ok(false);
        }
    }
    let ok = carry_out(&steps);
    #[cfg(unix)]
    println!(
        "Done. v4l2loopback stays loaded until you restart; `sudo apt autoremove` removes v4l2loopback-dkms if \
         nothing else uses it."
    );
    #[cfg(windows)]
    println!("Done.");
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_remove_files_and_folders_and_report_failures() {
        let dir = std::env::temp_dir().join(format!("opal-uninstall-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("settings/presets")).unwrap();
        std::fs::write(dir.join("settings/presets/evening"), "blur 24").unwrap();
        std::fs::write(dir.join("unit"), "x").unwrap();
        let steps = [Step::Remove(dir.join("settings")), Step::Remove(dir.join("unit"))];
        assert!(carry_out(&steps));
        assert!(!dir.join("settings").exists() && !dir.join("unit").exists());
        assert!(
            !carry_out(&[Step::Remove(dir.join("gone"))]),
            "a missing file is reported"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn plan_starts_by_stopping_and_keeps_data_when_asked() {
        let steps = plan(true);
        assert_eq!(steps[0], Step::Stop);
        let data = [crate::platform::config_dir(), crate::platform::data_dir()];
        assert!(!steps.iter().any(|s| matches!(s, Step::Remove(p) if data.contains(p))));
    }
}
