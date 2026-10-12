//! Builds the packages: `cargo xtask deb` (Ubuntu and Debian) and `cargo xtask exe` (Windows installer).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const USAGE: &str = "\
usage: cargo xtask <deb | exe [--target TRIPLE] | all>

  deb   Ubuntu/Debian package, needs cargo-deb (cargo install cargo-deb)
  exe   Windows installer with the programs built in, needs the Windows target
        (rustup target add x86_64-pc-windows-gnu) and, off Windows, MinGW
        (sudo apt install gcc-mingw-w64-x86-64)
  all   both

Both are written to target/dist. Neither holds any Opal files: installed copies
ask for the user's own disk image.";

const WINDOWS_TARGET: &str = "x86_64-pc-windows-gnu";
/// What the installer installs.
const WINDOWS_FILES: [&str; 4] = ["opal-rs.exe", "opal-rs-gui.exe", "opal-rs-tray.exe", "opal_vcam.dll"];

type Result<T = ()> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("deb") if args.len() == 1 => deb(),
        Some("exe") => target_arg(&args[1..]).and_then(|t| exe(&t)),
        Some("all") if args.len() == 1 => deb().and_then(|()| exe(WINDOWS_TARGET)),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn target_arg(args: &[String]) -> Result<String> {
    match args {
        [] => Ok(WINDOWS_TARGET.into()),
        [flag, t] if flag == "--target" => Ok(t.clone()),
        _ => Err(format!("unexpected arguments\n\n{USAGE}")),
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn target_dir() -> PathBuf {
    let dir = std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root().join("target"), PathBuf::from);
    std::path::absolute(&dir).unwrap_or(dir)
}

fn dist_dir() -> Result<PathBuf> {
    let dir = target_dir().join("dist");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// `cargo` with the source tree's `assets/` folder left out of the build, and no paths of this computer in the
/// programs (panic messages name source files).
fn cargo<I: IntoIterator<Item = S>, S: AsRef<OsStr>>(args: I) -> Command {
    let mut flags: Vec<String> = match std::env::var("CARGO_ENCODED_RUSTFLAGS") {
        Ok(f) => f.split('\x1f').filter(|f| !f.is_empty()).map(String::from).collect(),
        Err(_) => std::env::var("RUSTFLAGS")
            .unwrap_or_default()
            .split_whitespace()
            .map(String::from)
            .collect(),
    };
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".cargo")));
    // The last match wins, so the most specific comes last.
    for (from, to) in [(home, "~"), (cargo_home, "cargo"), (Some(root()), "opal-rs")] {
        if let Some(from) = from {
            flags.push(format!("--remap-path-prefix={}={to}", from.display()));
        }
    }
    let mut cmd = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cmd.current_dir(root())
        .env("OPAL_RS_PACKAGED", "1")
        .env("CARGO_ENCODED_RUSTFLAGS", flags.join("\x1f"))
        .env_remove("RUSTFLAGS")
        .args(args);
    cmd
}

fn packages<'a>(names: &[&'a str]) -> Vec<&'a str> {
    names.iter().flat_map(|n| ["-p", n]).collect()
}

fn run(cmd: &mut Command) -> Result {
    let status = cmd.status().map_err(|e| format!("{:?}: {e}", cmd.get_program()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{:?} failed ({status})", cmd.get_program()))
    }
}

fn found(program: &str, arg: &str) -> bool {
    Command::new(program)
        .arg(arg)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn copy(from: &Path, to: &Path) -> Result {
    std::fs::copy(from, to)
        .map(drop)
        .map_err(|e| format!("{} -> {}: {e}", from.display(), to.display()))
}

fn deb() -> Result {
    if !cfg!(target_os = "linux") {
        return Err("the .deb is built on Linux".into());
    }
    if !cargo(["deb", "--version"]).output().is_ok_and(|o| o.status.success()) {
        return Err("cargo-deb is missing; install it with: cargo install cargo-deb".into());
    }
    println!("building the programs");
    run(cargo(["build", "--release"]).args(packages(&["opal-rs", "opal-gui", "opal-tray"])))?;
    println!("building the package");
    let out = dist_dir()?;
    run(cargo(["deb", "-p", "opal-rs", "--no-build", "--output"]).arg(&out))?;
    println!("done: {}", out.display());
    Ok(())
}

fn exe(target: &str) -> Result {
    if !target.contains("windows") {
        return Err(format!("{target} is not a Windows target"));
    }
    if found("rustup", "--version") {
        let installed = Command::new("rustup").args(["target", "list", "--installed"]).output();
        if installed.is_ok_and(|o| !String::from_utf8_lossy(&o.stdout).lines().any(|l| l == target)) {
            return Err(format!(
                "the {target} target is missing; add it with: rustup target add {target}"
            ));
        }
    }
    if !cfg!(windows) && target.ends_with("-gnu") && !found("x86_64-w64-mingw32-gcc", "--version") {
        return Err("the MinGW linker is missing; on Ubuntu or Debian: sudo apt install gcc-mingw-w64-x86-64".into());
    }

    // Stripped programs keep the installer small.
    let build = |args: &[&str]| {
        let mut cmd = cargo(["build", "--release", "--target", target]);
        cmd.args(args).env("CARGO_PROFILE_RELEASE_STRIP", "symbols");
        cmd
    };
    println!("building the programs");
    run(build(&[]).args(packages(&["opal-rs", "opal-gui", "opal-tray", "opal-vcam"])))?;

    let release = target_dir().join(target).join("release");
    let payload = target_dir().join("payload");
    std::fs::create_dir_all(&payload).map_err(|e| format!("{}: {e}", payload.display()))?;
    for f in WINDOWS_FILES {
        copy(&release.join(f), &payload.join(f))?;
    }

    println!("building the installer");
    run(build(&["-p", "opal-setup", "--features", "payload"]).env("OPAL_RS_PAYLOAD", &payload))?;
    let out = dist_dir()?.join("opal-rs-setup.exe");
    copy(&release.join("opal-rs-setup.exe"), &out)?;
    println!("done: {}", out.display());
    Ok(())
}
