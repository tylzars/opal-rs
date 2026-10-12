//! Extracts the C1 camera files from an Opal Composer disk image.

pub mod hfs;
pub mod manifest;
pub mod udif;
pub mod xz;

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub use manifest::{ASSETS, Asset, Kind, RELEASES, Release, Source};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("{0}")]
    Format(String),
    #[error("{0} not found in disk image")]
    NotFound(String),
    #[error("{0}: checksum mismatch")]
    Mismatch(String),
}

pub enum Event<'a> {
    Identified(Option<&'static Release>),
    Wrote(&'a Asset, &'a Path),
}

/// Extracts and verifies every asset into `out_dir`.
pub fn extract(dmg: &Path, out_dir: &Path, mut on_event: impl FnMut(Event<'_>)) -> Result<(), Error> {
    let digest = sha256_file(dmg)?;
    on_event(Event::Identified(RELEASES.iter().find(|r| r.dmg_sha256 == digest)));

    let image = udif::UdifImage::open(dmg)?;
    let partition = image.open_partition("Apple_HFS")?;
    let mut volume = hfs::Volume::open(partition)?;

    fs::create_dir_all(out_dir)?;
    let mut archives: HashMap<&str, HashMap<String, Vec<u8>>> = HashMap::new();

    for asset in ASSETS {
        let bytes = match asset.source {
            Source::File(path) => volume.read_file(path)?,
            Source::EmbeddedArchive { container, member } => {
                if !archives.contains_key(container) {
                    let data = volume.read_file(container)?;
                    archives.insert(container, archive_members(&data)?);
                }
                archives[container]
                    .get(member)
                    .cloned()
                    .ok_or_else(|| Error::NotFound(format!("{container}: {member}")))?
            }
        };
        check(asset, &bytes)?;
        let dest = out_dir.join(asset.file_name);
        write_atomically(&dest, &bytes)?;
        on_event(Event::Wrote(asset, &dest));
    }
    Ok(())
}

pub enum Status {
    Ok,
    Missing,
    Mismatch(Error),
}

pub fn verify(dir: &Path) -> Result<Vec<(&'static Asset, Status)>, Error> {
    let mut results = Vec::new();
    for asset in ASSETS {
        let path = dir.join(asset.file_name);
        let status = match fs::read(&path) {
            Ok(bytes) => match check(asset, &bytes) {
                Ok(()) => Status::Ok,
                Err(e) => Status::Mismatch(e),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => Status::Missing,
            Err(e) => return Err(e.into()),
        };
        results.push((asset, status));
    }
    Ok(results)
}

/// Reads an asset, failing if it does not match the manifest.
pub fn load(dir: &Path, asset: &Asset) -> Result<Vec<u8>, Error> {
    let bytes = fs::read(dir.join(asset.file_name))?;
    check(asset, &bytes)?;
    Ok(bytes)
}

fn check(asset: &Asset, bytes: &[u8]) -> Result<(), Error> {
    let actual = hex(&Sha256::digest(bytes));
    if bytes.len() as u64 != asset.size || actual != asset.sha256 {
        return Err(Error::Mismatch(asset.file_name.to_owned()));
    }
    Ok(())
}

/// Files in every xz-compressed tar embedded in `data`.
fn archive_members(data: &[u8]) -> Result<HashMap<String, Vec<u8>>, Error> {
    let mut members = HashMap::new();
    for stream in xz::embedded_streams(data) {
        let mut archive = tar::Archive::new(stream.decoded.as_slice());
        let Ok(entries) = archive.entries() else { continue };
        for entry in entries {
            let Ok(mut entry) = entry else { break };
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let name = entry.path()?.to_string_lossy().into_owned();
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents)?;
            members.entry(name).or_insert(contents);
        }
    }
    Ok(members)
}

fn write_atomically(dest: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut tmp = PathBuf::from(dest);
    tmp.as_mut_os_string().push(".partial");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, dest)?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, Error> {
    let mut f = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
