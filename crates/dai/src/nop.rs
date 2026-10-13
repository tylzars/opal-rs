//! libnop binary format (github.com/google/libnop, docs/format.md).

use std::collections::BTreeMap;

const U8: u8 = 0x80;
const U16: u8 = 0x81;
const U32: u8 = 0x82;
const U64: u8 = 0x83;
const I8: u8 = 0x84;
const I16: u8 = 0x85;
const I32: u8 = 0x86;
const I64: u8 = 0x87;
const F32: u8 = 0x88;
const F64: u8 = 0x89;
const STRUCTURE: u8 = 0xb9;
const ARRAY: u8 = 0xba;
const MAP: u8 = 0xbb;
const BINARY: u8 = 0xbc;
const STRING: u8 = 0xbd;
const EMPTY: u8 = 0xbe;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("unexpected end of data")]
    Eof,
    #[error("unexpected prefix {0:#04x}")]
    Prefix(u8),
    #[error("expected {expected} members, got {got}")]
    Members { expected: u64, got: u64 },
    #[error("value out of range")]
    Range,
    #[error("invalid utf-8")]
    Utf8,
}

pub struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    fn byte(&mut self) -> Result<u8, Error> {
        let (&b, rest) = self.data.split_first().ok_or(Error::Eof)?;
        self.data = rest;
        Ok(b)
    }

    fn peek(&self) -> Result<u8, Error> {
        self.data.first().copied().ok_or(Error::Eof)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.data.len() < n {
            return Err(Error::Eof);
        }
        let (a, b) = self.data.split_at(n);
        self.data = b;
        Ok(a)
    }

    fn le<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        Ok(self.take(N)?.try_into().unwrap())
    }

    /// Reads any integer encoding as i128.
    fn int(&mut self) -> Result<i128, Error> {
        let p = self.byte()?;
        Ok(match p {
            0x00..=0x7f => p as i128,
            0xc0..=0xff => p as i8 as i128,
            U8 => self.le::<1>()?[0] as i128,
            U16 => u16::from_le_bytes(self.le()?) as i128,
            U32 => u32::from_le_bytes(self.le()?) as i128,
            U64 => u64::from_le_bytes(self.le()?) as i128,
            I8 => self.le::<1>()?[0] as i8 as i128,
            I16 => i16::from_le_bytes(self.le()?) as i128,
            I32 => i32::from_le_bytes(self.le()?) as i128,
            I64 => i64::from_le_bytes(self.le()?) as i128,
            _ => return Err(Error::Prefix(p)),
        })
    }

    fn len(&mut self) -> Result<usize, Error> {
        usize::try_from(self.int()?).map_err(|_| Error::Range)
    }

    fn expect(&mut self, prefix: u8) -> Result<(), Error> {
        match self.byte()? {
            p if p == prefix => Ok(()),
            p => Err(Error::Prefix(p)),
        }
    }

    /// Reads a structure header and checks its member count.
    pub fn structure(&mut self, members: u64) -> Result<(), Error> {
        self.expect(STRUCTURE)?;
        let got = self.len()? as u64;
        if got != members {
            return Err(Error::Members { expected: members, got });
        }
        Ok(())
    }
}

pub trait Nop: Sized {
    fn encode(&self, out: &mut Vec<u8>);
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error>;

    /// Encoding of a sequence of `Self`. Integral types override this to use a binary blob.
    fn encode_seq(items: &[Self], out: &mut Vec<u8>) {
        out.push(ARRAY);
        (items.len() as u64).encode(out);
        for item in items {
            item.encode(out);
        }
    }

    fn decode_seq(r: &mut Reader<'_>) -> Result<Vec<Self>, Error> {
        r.expect(ARRAY)?;
        let n = r.len()?;
        (0..n).map(|_| Self::decode(r)).collect()
    }
}

pub fn to_vec<T: Nop>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

pub fn from_slice<T: Nop>(data: &[u8]) -> Result<T, Error> {
    T::decode(&mut Reader::new(data))
}

fn encode_unsigned(v: u64, max: u8, out: &mut Vec<u8>) {
    if v < 0x80 {
        out.push(v as u8);
    } else if v <= u8::MAX as u64 {
        out.extend_from_slice(&[U8, v as u8]);
    } else if v <= u16::MAX as u64 && max >= U16 {
        out.push(U16);
        out.extend_from_slice(&(v as u16).to_le_bytes());
    } else if v <= u32::MAX as u64 && max >= U32 {
        out.push(U32);
        out.extend_from_slice(&(v as u32).to_le_bytes());
    } else {
        out.push(U64);
        out.extend_from_slice(&v.to_le_bytes());
    }
}

fn encode_signed(v: i64, out: &mut Vec<u8>) {
    if (-64..=127).contains(&v) {
        out.push(v as i8 as u8);
    } else if i8::try_from(v).is_ok() {
        out.extend_from_slice(&[I8, v as i8 as u8]);
    } else if let Ok(v) = i16::try_from(v) {
        out.push(I16);
        out.extend_from_slice(&v.to_le_bytes());
    } else if let Ok(v) = i32::try_from(v) {
        out.push(I32);
        out.extend_from_slice(&v.to_le_bytes());
    } else {
        out.push(I64);
        out.extend_from_slice(&v.to_le_bytes());
    }
}

macro_rules! integral {
    ($($t:ty => $enc:expr),* $(,)?) => {$(
        impl Nop for $t {
            fn encode(&self, out: &mut Vec<u8>) {
                let f: fn($t, &mut Vec<u8>) = $enc;
                f(*self, out)
            }
            fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
                <$t>::try_from(r.int()?).map_err(|_| Error::Range)
            }
            fn encode_seq(items: &[Self], out: &mut Vec<u8>) {
                out.push(BINARY);
                ((items.len() * size_of::<$t>()) as u64).encode(out);
                for v in items {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
            fn decode_seq(r: &mut Reader<'_>) -> Result<Vec<Self>, Error> {
                r.expect(BINARY)?;
                let n = r.len()?;
                if n % size_of::<$t>() != 0 {
                    return Err(Error::Range);
                }
                Ok(r.take(n)?
                    .chunks_exact(size_of::<$t>())
                    .map(|c| <$t>::from_le_bytes(c.try_into().unwrap()))
                    .collect())
            }
        }
    )*};
}

integral! {
    u8 => |v, o| encode_unsigned(v as u64, U8, o),
    u16 => |v, o| encode_unsigned(v as u64, U16, o),
    u32 => |v, o| encode_unsigned(v as u64, U32, o),
    u64 => |v, o| encode_unsigned(v, U64, o),
    i8 => |v, o| encode_signed(v as i64, o),
    i16 => |v, o| encode_signed(v as i64, o),
    i32 => |v, o| encode_signed(v as i64, o),
    i64 => |v, o| encode_signed(v, o),
}

impl Nop for bool {
    fn encode(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        match r.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            p => Err(Error::Prefix(p)),
        }
    }
    fn encode_seq(items: &[Self], out: &mut Vec<u8>) {
        out.push(BINARY);
        (items.len() as u64).encode(out);
        out.extend(items.iter().map(|&b| b as u8));
    }
    fn decode_seq(r: &mut Reader<'_>) -> Result<Vec<Self>, Error> {
        Ok(u8::decode_seq(r)?.into_iter().map(|b| b != 0).collect())
    }
}

impl Nop for f32 {
    fn encode(&self, out: &mut Vec<u8>) {
        out.push(F32);
        out.extend_from_slice(&self.to_le_bytes());
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        r.expect(F32)?;
        Ok(f32::from_le_bytes(r.le()?))
    }
}

impl Nop for f64 {
    fn encode(&self, out: &mut Vec<u8>) {
        out.push(F64);
        out.extend_from_slice(&self.to_le_bytes());
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        r.expect(F64)?;
        Ok(f64::from_le_bytes(r.le()?))
    }
}

impl Nop for String {
    fn encode(&self, out: &mut Vec<u8>) {
        out.push(STRING);
        (self.len() as u64).encode(out);
        out.extend_from_slice(self.as_bytes());
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        r.expect(STRING)?;
        let n = r.len()?;
        String::from_utf8(r.take(n)?.to_vec()).map_err(|_| Error::Utf8)
    }
}

impl<T: Nop> Nop for Vec<T> {
    fn encode(&self, out: &mut Vec<u8>) {
        T::encode_seq(self, out)
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        T::decode_seq(r)
    }
}

impl<T: Nop, const N: usize> Nop for [T; N] {
    fn encode(&self, out: &mut Vec<u8>) {
        T::encode_seq(self, out)
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        T::decode_seq(r)?.try_into().map_err(|_| Error::Range)
    }
}

/// An array that is always empty, for fields that are never used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmptyArray;

impl Nop for EmptyArray {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[ARRAY, 0]);
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        r.expect(ARRAY)?;
        match r.len()? {
            0 => Ok(EmptyArray),
            _ => Err(Error::Range),
        }
    }
}

impl<T: Nop> Nop for Option<T> {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Some(v) => v.encode(out),
            None => out.push(EMPTY),
        }
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        if r.peek()? == EMPTY {
            r.byte()?;
            return Ok(None);
        }
        T::decode(r).map(Some)
    }
}

impl<K: Nop + Ord, V: Nop> Nop for BTreeMap<K, V> {
    fn encode(&self, out: &mut Vec<u8>) {
        out.push(MAP);
        (self.len() as u64).encode(out);
        for (k, v) in self {
            k.encode(out);
            v.encode(out);
        }
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        r.expect(MAP)?;
        let n = r.len()?;
        (0..n).map(|_| Ok((K::decode(r)?, V::decode(r)?))).collect()
    }
}

impl<A: Nop, B: Nop> Nop for (A, B) {
    fn encode(&self, out: &mut Vec<u8>) {
        out.push(ARRAY);
        2u64.encode(out);
        self.0.encode(out);
        self.1.encode(out);
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        r.expect(ARRAY)?;
        if r.len()? != 2 {
            return Err(Error::Range);
        }
        Ok((A::decode(r)?, B::decode(r)?))
    }
}

/// Writes a structure header for `members` fields.
pub fn structure(members: u64, out: &mut Vec<u8>) {
    out.push(STRUCTURE);
    members.encode(out);
}

/// Implements [`Nop`] for a struct as a libnop structure with the listed fields, in order.
#[macro_export]
macro_rules! nop_struct {
    ($t:ty { $($field:ident),* $(,)? }) => {
        impl $crate::nop::Nop for $t {
            fn encode(&self, out: &mut Vec<u8>) {
                $crate::nop::structure([$(stringify!($field)),*].len() as u64, out);
                $( $crate::nop::Nop::encode(&self.$field, out); )*
            }
            fn decode(r: &mut $crate::nop::Reader<'_>) -> Result<Self, $crate::nop::Error> {
                r.structure([$(stringify!($field)),*].len() as u64)?;
                Ok(Self { $( $field: $crate::nop::Nop::decode(r)?, )* })
            }
        }
    };
}

/// Implements [`Nop`] for a fieldless enum as its integer representation.
#[macro_export]
macro_rules! nop_enum {
    ($t:ty as $repr:ty { $($variant:ident),* $(,)? }) => {
        impl $crate::nop::Nop for $t {
            fn encode(&self, out: &mut Vec<u8>) {
                $crate::nop::Nop::encode(&(*self as $repr), out)
            }
            fn decode(r: &mut $crate::nop::Reader<'_>) -> Result<Self, $crate::nop::Error> {
                let v = <$repr as $crate::nop::Nop>::decode(r)?;
                $( if v == <$t>::$variant as $repr { return Ok(<$t>::$variant); } )*
                Err($crate::nop::Error::Range)
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_use_smallest_encoding() {
        assert_eq!(to_vec(&5u32), [5]);
        assert_eq!(to_vec(&200u32), [U8, 200]);
        assert_eq!(to_vec(&0x1234u32), [U16, 0x34, 0x12]);
        assert_eq!(to_vec(&0x12345678u32), [U32, 0x78, 0x56, 0x34, 0x12]);
        assert_eq!(to_vec(&-1i32), [0xff]);
        assert_eq!(to_vec(&-64i32), [0xc0]);
        assert_eq!(to_vec(&-65i32), [I8, 0xbf]);
        assert_eq!(to_vec(&127i32), [0x7f]);
        assert_eq!(to_vec(&128i32), [I16, 0x80, 0x00]);
        assert_eq!(to_vec(&-40000i32), [I32, 0xc0, 0x63, 0xff, 0xff]);
    }

    #[test]
    fn containers() {
        assert_eq!(to_vec(&vec![1u16, 2]), [BINARY, 4, 1, 0, 2, 0]);
        assert_eq!(to_vec(&vec![1.0f32]), [ARRAY, 1, F32, 0, 0, 0x80, 0x3f]);
        assert_eq!(to_vec(&"ab".to_string()), [STRING, 2, b'a', b'b']);
        assert_eq!(to_vec(&None::<u32>), [EMPTY]);
        assert_eq!(to_vec(&Some(3u32)), [3]);
        assert_eq!(to_vec(&(1u8, 2u8)), [ARRAY, 2, 1, 2]);
    }

    #[derive(Debug, PartialEq)]
    struct Point {
        x: i32,
        y: Vec<u8>,
        z: Option<String>,
    }
    crate::nop_struct!(Point { x, y, z });

    #[test]
    fn structure_round_trip() {
        let p = Point {
            x: -3,
            y: vec![9, 8],
            z: Some("q".into()),
        };
        let bytes = to_vec(&p);
        assert_eq!(bytes, [STRUCTURE, 3, 0xfd, BINARY, 2, 9, 8, STRING, 1, b'q']);
        assert_eq!(from_slice::<Point>(&bytes).unwrap(), p);
    }

    #[test]
    fn decoder_accepts_wider_encodings() {
        assert_eq!(from_slice::<u32>(&[U16, 5, 0]).unwrap(), 5);
        assert_eq!(from_slice::<i8>(&[I16, 0x00, 0x01]), Err(Error::Range));
    }
}
