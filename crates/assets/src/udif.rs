//! Reader for UDIF (.dmg) disk images.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use flate2::read::ZlibDecoder;

use crate::Error;

const SECTOR: u64 = 512;
const KOLY_LEN: u64 = 512;

const CHUNK_ZERO: u32 = 0x0000_0000;
const CHUNK_RAW: u32 = 0x0000_0001;
const CHUNK_IGNORE: u32 = 0x0000_0002;
const CHUNK_COMMENT: u32 = 0x7fff_fffe;
const CHUNK_ZLIB: u32 = 0x8000_0005;
const CHUNK_END: u32 = 0xffff_ffff;

pub struct UdifImage {
    file: File,
    data_fork_offset: u64,
    partitions: Vec<PartitionTable>,
}

struct PartitionTable {
    name: String,
    data_offset: u64,
    chunks: Vec<Chunk>,
}

#[derive(Clone, Copy, Debug)]
struct Chunk {
    kind: u32,
    /// Relative to the partition.
    sector: u64,
    sectors: u64,
    /// Relative to the partition data offset.
    offset: u64,
    length: u64,
}

impl UdifImage {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let mut file = File::open(path)?;
        let len = file.seek(SeekFrom::End(0))?;
        if len < KOLY_LEN {
            return Err(Error::Format("not a disk image".into()));
        }
        let mut koly = [0u8; KOLY_LEN as usize];
        file.seek(SeekFrom::Start(len - KOLY_LEN))?;
        file.read_exact(&mut koly)?;
        if &koly[0..4] != b"koly" {
            return Err(Error::Format("not a disk image".into()));
        }
        let data_fork_offset = be_u64(&koly, 0x18);
        let xml_offset = be_u64(&koly, 0xd8);
        let xml_length = be_u64(&koly, 0xe0);
        if xml_length == 0 || xml_offset.checked_add(xml_length).is_none_or(|end| end > len) {
            return Err(Error::Format("bad UDIF trailer".into()));
        }

        let mut xml = vec![0u8; xml_length as usize];
        file.seek(SeekFrom::Start(xml_offset))?;
        file.read_exact(&mut xml)?;
        let partitions = parse_partition_tables(&xml)?;

        Ok(Self {
            file,
            data_fork_offset,
            partitions,
        })
    }

    pub fn open_partition(&self, pattern: &str) -> Result<Partition, Error> {
        let table = self
            .partitions
            .iter()
            .find(|p| p.name.contains(pattern))
            .ok_or_else(|| Error::Format(format!("no partition matching {pattern:?}")))?;
        let mut chunks: Vec<Chunk> = table
            .chunks
            .iter()
            .copied()
            .filter(|c| !matches!(c.kind, CHUNK_COMMENT | CHUNK_END) && c.sectors > 0)
            .collect();
        chunks.sort_by_key(|c| c.sector);
        for c in &chunks {
            if !matches!(c.kind, CHUNK_ZERO | CHUNK_RAW | CHUNK_IGNORE | CHUNK_ZLIB) {
                return Err(Error::Format(format!("unsupported chunk type {:#x}", c.kind)));
            }
        }
        let size = chunks.last().map_or(0, |c| (c.sector + c.sectors) * SECTOR);
        Ok(Partition {
            file: BufReader::new(self.file.try_clone()?),
            base: self.data_fork_offset + table.data_offset,
            chunks,
            size,
            pos: 0,
            cached: None,
        })
    }
}

fn parse_partition_tables(xml: &[u8]) -> Result<Vec<PartitionTable>, Error> {
    let root: plist::Value = plist::from_bytes(xml).map_err(|e| Error::Format(format!("bad UDIF plist: {e}")))?;
    let blkx = root
        .as_dictionary()
        .and_then(|d| d.get("resource-fork"))
        .and_then(|v| v.as_dictionary())
        .and_then(|d| d.get("blkx"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::Format("no blkx table".into()))?;

    let mut tables = Vec::new();
    for entry in blkx {
        let Some(dict) = entry.as_dictionary() else { continue };
        let name = dict
            .get("Name")
            .and_then(|v| v.as_string())
            .unwrap_or_default()
            .to_owned();
        let data = dict
            .get("Data")
            .and_then(|v| v.as_data())
            .ok_or_else(|| Error::Format(format!("blkx {name:?}: no data")))?;
        tables.push(parse_mish(name, data)?);
    }
    Ok(tables)
}

/// 204-byte header, then 40-byte chunk entries.
fn parse_mish(name: String, data: &[u8]) -> Result<PartitionTable, Error> {
    if data.len() < 204 || &data[0..4] != b"mish" {
        return Err(Error::Format(format!("blkx {name:?}: bad mish table")));
    }
    let data_offset = be_u64(data, 24);
    let count = be_u32(data, 200) as usize;
    if data.len() < 204 + count * 40 {
        return Err(Error::Format(format!("blkx {name:?}: truncated")));
    }
    let chunks = (0..count)
        .map(|i| {
            let e = &data[204 + i * 40..];
            Chunk {
                kind: be_u32(e, 0),
                sector: be_u64(e, 8),
                sectors: be_u64(e, 16),
                offset: be_u64(e, 24),
                length: be_u64(e, 32),
            }
        })
        .collect();
    Ok(PartitionTable {
        name,
        data_offset,
        chunks,
    })
}

/// Uncompressed view of one partition.
pub struct Partition {
    file: BufReader<File>,
    base: u64,
    chunks: Vec<Chunk>,
    size: u64,
    pos: u64,
    cached: Option<(usize, Vec<u8>)>,
}

impl Partition {
    fn chunk_index(&self, pos: u64) -> Option<usize> {
        let sector = pos / SECTOR;
        let i = self.chunks.partition_point(|c| c.sector + c.sectors <= sector);
        (i < self.chunks.len() && self.chunks[i].sector <= sector).then_some(i)
    }

    fn load(&mut self, index: usize) -> io::Result<&[u8]> {
        if self.cached.as_ref().is_none_or(|(i, _)| *i != index) {
            let c = self.chunks[index];
            let expected = (c.sectors * SECTOR) as usize;
            let bytes = match c.kind {
                CHUNK_ZERO | CHUNK_IGNORE => vec![0u8; expected],
                CHUNK_RAW | CHUNK_ZLIB => {
                    self.file.seek(SeekFrom::Start(self.base + c.offset))?;
                    let mut stored = (&mut self.file).take(c.length);
                    let mut out = Vec::with_capacity(expected);
                    if c.kind == CHUNK_RAW {
                        stored.read_to_end(&mut out)?;
                    } else {
                        ZlibDecoder::new(stored).read_to_end(&mut out)?;
                    }
                    if out.len() != expected {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad chunk length"));
                    }
                    out
                }
                _ => unreachable!("rejected in open_partition"),
            };
            self.cached = Some((index, bytes));
        }
        Ok(&self.cached.as_ref().expect("just filled").1)
    }
}

impl Read for Partition {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.size {
            return Ok(0);
        }
        let pos = self.pos;
        let Some(index) = self.chunk_index(pos) else {
            // gaps read as zeros
            let next = self
                .chunks
                .iter()
                .map(|c| c.sector * SECTOR)
                .find(|&s| s > pos)
                .unwrap_or(self.size);
            let n = buf.len().min((next - pos) as usize);
            buf[..n].fill(0);
            self.pos += n as u64;
            return Ok(n);
        };
        let start = self.chunks[index].sector * SECTOR;
        let bytes = self.load(index)?;
        let within = (pos - start) as usize;
        let n = buf.len().min(bytes.len() - within);
        buf[..n].copy_from_slice(&bytes[within..within + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Partition {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(d) => self.size.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos = target.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
        Ok(self.pos)
    }
}

fn be_u32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

fn be_u64(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}
