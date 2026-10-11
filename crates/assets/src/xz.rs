//! Finds xz streams embedded in other files.

use std::io::Cursor;

const HEADER_MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0x00];
const FOOTER_MAGIC: [u8; 2] = *b"YZ";
const HEADER_LEN: usize = 12;
const FOOTER_LEN: usize = 12;

pub struct EmbeddedStream {
    pub offset: usize,
    pub len: usize,
    pub decoded: Vec<u8>,
}

/// Every valid xz stream in `data`, in order.
pub fn embedded_streams(data: &[u8]) -> impl Iterator<Item = EmbeddedStream> + '_ {
    let mut search_from = 0;
    std::iter::from_fn(move || {
        while let Some(start) = find(&data[search_from..], &HEADER_MAGIC).map(|p| p + search_from) {
            search_from = start + 1;
            if let Some(stream) = decode_at(data, start) {
                search_from = stream.offset + stream.len;
                return Some(stream);
            }
        }
        None
    })
}

fn decode_at(data: &[u8], start: usize) -> Option<EmbeddedStream> {
    let header = data.get(start..start + HEADER_LEN)?;
    let flags = &header[6..8];
    if crc32fast::hash(flags) != le_u32(header, 8) {
        return None;
    }

    let mut cursor = start + HEADER_LEN;
    while let Some(p) = find(&data[cursor..], &FOOTER_MAGIC) {
        let end = cursor + p + FOOTER_MAGIC.len();
        cursor = end - 1;
        if end - start < HEADER_LEN + FOOTER_LEN {
            continue;
        }
        let footer = &data[end - FOOTER_LEN..end];
        if &footer[8..10] != flags || crc32fast::hash(&footer[4..10]) != le_u32(footer, 0) {
            continue;
        }
        let mut decoded = Vec::new();
        if lzma_rs::xz_decompress(&mut Cursor::new(&data[start..end]), &mut decoded).is_ok() {
            return Some(EmbeddedStream {
                offset: start,
                len: end - start,
                decoded,
            });
        }
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compress(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        lzma_rs::xz_compress(&mut Cursor::new(data), &mut out).unwrap();
        out
    }

    #[test]
    fn finds_streams_between_unrelated_bytes() {
        let first = b"first payload ".repeat(100);
        let second = b"second payload".repeat(50);

        let mut file = b"leading bytes, then a fake magic: ".to_vec();
        file.extend_from_slice(&HEADER_MAGIC);
        file.extend_from_slice(b"\x00\x01 junk YZ junk ");
        file.extend_from_slice(&compress(&first));
        file.extend_from_slice(b" between YZ ");
        file.extend_from_slice(&compress(&second));
        file.extend_from_slice(b"trailing bytes");

        let found: Vec<_> = embedded_streams(&file).map(|s| s.decoded).collect();
        assert_eq!(found, vec![first, second]);
    }

    #[test]
    fn ignores_truncated_stream() {
        let mut stream = compress(b"payload");
        stream.truncate(stream.len() - 3);
        assert_eq!(embedded_streams(&stream).count(), 0);
    }
}
