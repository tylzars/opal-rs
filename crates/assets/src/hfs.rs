//! Read-only HFS Plus reader (Apple TN1150).

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

use unicode_normalization::UnicodeNormalization;

use crate::Error;

const VOLUME_HEADER_OFFSET: u64 = 1024;

const ROOT_FOLDER_ID: u32 = 2;
const EXTENTS_FILE_ID: u32 = 3;
const CATALOG_FILE_ID: u32 = 4;

const RECORD_FOLDER: u16 = 1;
const RECORD_FILE: u16 = 2;

const NODE_KIND_LEAF: i8 = -1;

const FORK_DATA: u8 = 0x00;

/// Holds hard link targets.
const PRIVATE_DATA_FOLDER: &str = "\0\0\0\0HFS+ Private Data";

const S_IFMT: u16 = 0o170000;
const S_IFREG: u16 = 0o100000;
const S_IFLNK: u16 = 0o120000;

const UF_COMPRESSED: u8 = 0x20;

#[derive(Clone, Debug, Default)]
struct ForkData {
    logical_size: u64,
    total_blocks: u32,
    extents: Vec<(u32, u32)>,
}

impl ForkData {
    fn parse(b: &[u8]) -> Self {
        let extents = (0..8)
            .map(|i| (be_u32(b, 16 + i * 8), be_u32(b, 20 + i * 8)))
            .filter(|&(_, count)| count > 0)
            .collect();
        Self {
            logical_size: be_u64(b, 0),
            total_blocks: be_u32(b, 12),
            extents,
        }
    }

    fn listed_blocks(&self) -> u64 {
        self.extents.iter().map(|&(_, n)| u64::from(n)).sum()
    }
}

#[derive(Clone, Debug)]
enum Entry {
    Folder { id: u32 },
    File(FileRecord),
}

#[derive(Clone, Debug)]
struct FileRecord {
    id: u32,
    mode: u16,
    owner_flags: u8,
    /// iNode number for hard links.
    link_target: Option<u32>,
    data: ForkData,
}

pub struct Volume<R> {
    reader: R,
    block_size: u64,
    /// (parent ID, NFD name) -> entry
    catalog: HashMap<(u32, String), Entry>,
    /// (file ID, fork) -> overflow extent records
    overflow: OverflowMap,
}

impl<R: Read + Seek> Volume<R> {
    pub fn open(mut reader: R) -> Result<Self, Error> {
        let mut header = [0u8; 512];
        reader.seek(SeekFrom::Start(VOLUME_HEADER_OFFSET))?;
        reader.read_exact(&mut header)?;
        if &header[0..2] != b"H+" && &header[0..2] != b"HX" {
            return Err(Error::Format("not an HFS Plus volume".into()));
        }
        let block_size = u64::from(be_u32(&header, 40));
        if !block_size.is_power_of_two() || block_size < 512 {
            return Err(Error::Format(format!("bad block size {block_size}")));
        }
        let extents_fork = ForkData::parse(&header[192..272]);
        let catalog_fork = ForkData::parse(&header[272..352]);

        let mut volume = Self {
            reader,
            block_size,
            catalog: HashMap::new(),
            overflow: HashMap::new(),
        };

        // the extents file never overflows itself
        let extents_tree = volume.read_fork(EXTENTS_FILE_ID, &extents_fork)?;
        volume.overflow = parse_extents_tree(&extents_tree)?;

        let catalog_tree = volume.read_fork(CATALOG_FILE_ID, &catalog_fork)?;
        volume.catalog = parse_catalog_tree(&catalog_tree)?;
        Ok(volume)
    }

    /// Reads a file's data fork. `path` is relative to the root folder.
    pub fn read_file(&mut self, path: &str) -> Result<Vec<u8>, Error> {
        let mut parent = ROOT_FOLDER_ID;
        let mut components = path.split('/').filter(|c| !c.is_empty()).peekable();
        while let Some(name) = components.next() {
            let entry = self
                .lookup(parent, name)
                .ok_or_else(|| Error::NotFound(path.to_owned()))?;
            match (entry, components.peek().is_some()) {
                (Entry::Folder { id }, true) => parent = id,
                (Entry::File(file), false) => return self.read_regular_file(path, file),
                _ => return Err(Error::NotFound(path.to_owned())),
            }
        }
        Err(Error::NotFound(path.to_owned()))
    }

    fn lookup(&self, parent: u32, name: &str) -> Option<Entry> {
        let key = (parent, name.nfd().collect::<String>());
        self.catalog.get(&key).cloned()
    }

    fn read_regular_file(&mut self, path: &str, mut file: FileRecord) -> Result<Vec<u8>, Error> {
        if let Some(inode) = file.link_target {
            let private = match self.lookup(ROOT_FOLDER_ID, PRIVATE_DATA_FOLDER) {
                Some(Entry::Folder { id }) => id,
                _ => {
                    return Err(Error::Format(format!("{path}: broken hard link")));
                }
            };
            file = match self.lookup(private, &format!("iNode{inode}")) {
                Some(Entry::File(target)) => target,
                _ => {
                    return Err(Error::Format(format!("{path}: broken hard link")));
                }
            };
        }
        match file.mode & S_IFMT {
            S_IFREG | 0 => {}
            S_IFLNK => return Err(Error::Format(format!("{path}: symlink"))),
            other => return Err(Error::Format(format!("{path}: not a file ({other:o})"))),
        }
        if file.owner_flags & UF_COMPRESSED != 0 {
            return Err(Error::Format(format!("{path}: compressed files unsupported")));
        }
        self.read_fork(file.id, &file.data)
    }

    fn read_fork(&mut self, file_id: u32, fork: &ForkData) -> Result<Vec<u8>, Error> {
        let mut extents = fork.extents.clone();
        if fork.listed_blocks() < u64::from(fork.total_blocks) {
            let mut more = self.overflow.get(&(file_id, FORK_DATA)).cloned().unwrap_or_default();
            more.sort_by_key(|(start, _)| *start);
            for (_, records) in more {
                extents.extend(records);
            }
        }
        let available: u64 = extents.iter().map(|&(_, n)| u64::from(n)).sum::<u64>() * self.block_size;
        if available < fork.logical_size {
            return Err(Error::Format(format!("file {file_id}: missing extents")));
        }

        let mut out = Vec::with_capacity(fork.logical_size as usize);
        for (start, count) in extents {
            let remaining = fork.logical_size - out.len() as u64;
            if remaining == 0 {
                break;
            }
            let len = (u64::from(count) * self.block_size).min(remaining);
            self.reader.seek(SeekFrom::Start(u64::from(start) * self.block_size))?;
            let before = out.len();
            (&mut self.reader).take(len).read_to_end(&mut out)?;
            if (out.len() - before) as u64 != len {
                return Err(Error::Format(format!("file {file_id}: past end of volume")));
            }
        }
        Ok(out)
    }
}

fn for_each_leaf_record(tree: &[u8], mut visit: impl FnMut(&[u8], &[u8]) -> Result<(), Error>) -> Result<(), Error> {
    if tree.len() < 14 + 106 {
        return Err(Error::Format("B-tree too small".into()));
    }
    // node 0: 14-byte descriptor, then the header record
    let first_leaf = be_u32(tree, 14 + 10);
    let node_size = usize::from(be_u16(tree, 14 + 18));
    let total_nodes = be_u32(tree, 14 + 22) as usize;
    if !node_size.is_power_of_two() || node_size < 512 {
        return Err(Error::Format(format!("bad B-tree node size {node_size}")));
    }

    let mut node_number = first_leaf as usize;
    let mut visited = 0usize;
    while node_number != 0 {
        visited += 1;
        if visited > total_nodes {
            return Err(Error::Format("B-tree loop".into()));
        }
        let node = tree
            .get(node_number * node_size..(node_number + 1) * node_size)
            .ok_or_else(|| Error::Format("B-tree node out of range".into()))?;
        if node[8] as i8 != NODE_KIND_LEAF {
            return Err(Error::Format("B-tree leaf chain broken".into()));
        }
        let records = usize::from(be_u16(node, 10));
        // record offsets are stored backwards from the end of the node
        for i in 0..records {
            let offset = usize::from(be_u16(node, node_size - 2 * (i + 1)));
            let key_len = usize::from(be_u16(node, offset));
            let key_end = offset + 2 + key_len;
            let record_start = key_end + (key_end & 1);
            if record_start > node_size {
                return Err(Error::Format("B-tree record out of range".into()));
            }
            visit(&node[offset + 2..key_end], &node[record_start..])?;
        }
        node_number = be_u32(node, 0) as usize;
    }
    Ok(())
}

fn parse_catalog_tree(tree: &[u8]) -> Result<HashMap<(u32, String), Entry>, Error> {
    let mut catalog = HashMap::new();
    for_each_leaf_record(tree, |key, record| {
        // key: parent ID, then UTF-16BE name
        let parent = be_u32(key, 0);
        let name_len = usize::from(be_u16(key, 4));
        let units: Vec<u16> = (0..name_len).map(|i| be_u16(key, 6 + i * 2)).collect();
        let name: String = String::from_utf16_lossy(&units).nfd().collect();
        let entry = match be_u16(record, 0) {
            RECORD_FOLDER => Entry::Folder { id: be_u32(record, 8) },
            RECORD_FILE => {
                let file_type = &record[48..52];
                let creator = &record[52..56];
                let special = be_u32(record, 44);
                Entry::File(FileRecord {
                    id: be_u32(record, 8),
                    mode: be_u16(record, 42),
                    owner_flags: record[41],
                    link_target: (file_type == b"hlnk" && creator == b"hfs+").then_some(special),
                    data: ForkData::parse(&record[88..168]),
                })
            }
            _ => return Ok(()), // thread records
        };
        catalog.insert((parent, name), entry);
        Ok(())
    })?;
    Ok(catalog)
}

type OverflowMap = HashMap<(u32, u8), Vec<(u32, Vec<(u32, u32)>)>>;

fn parse_extents_tree(tree: &[u8]) -> Result<OverflowMap, Error> {
    let mut map: OverflowMap = HashMap::new();
    if tree.is_empty() {
        return Ok(map);
    }
    for_each_leaf_record(tree, |key, record| {
        // key: fork type, pad, file ID, start block
        let fork_type = key[0];
        let file_id = be_u32(key, 2);
        let start_block = be_u32(key, 6);
        let extents = (0..8)
            .map(|i| (be_u32(record, i * 8), be_u32(record, i * 8 + 4)))
            .filter(|&(_, n)| n > 0)
            .collect();
        map.entry((file_id, fork_type))
            .or_default()
            .push((start_block, extents));
        Ok(())
    })?;
    Ok(map)
}

fn be_u16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes(b[at..at + 2].try_into().unwrap())
}

fn be_u32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

fn be_u64(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}
