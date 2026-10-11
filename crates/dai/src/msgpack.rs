//! MessagePack values (msgpack.org spec), used by the RPC channel.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    UInt(u64),
    F32(f32),
    F64(f64),
    Str(String),
    Array(Vec<Value>),
    /// String-keyed map, written with keys sorted.
    Map(BTreeMap<String, Value>),
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("unexpected end of data")]
    Eof,
    #[error("unsupported msgpack type {0:#04x}")]
    Type(u8),
}

impl Value {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut Vec<u8>) {
        match self {
            Value::Nil => out.push(0xc0),
            Value::Bool(b) => out.push(if *b { 0xc3 } else { 0xc2 }),
            Value::UInt(v) => write_uint(*v, out),
            Value::Int(v) if *v >= 0 => write_uint(*v as u64, out),
            Value::Int(v) => {
                let v = *v;
                if v >= -32 {
                    out.push(v as i8 as u8);
                } else if v >= i8::MIN as i64 {
                    out.extend_from_slice(&[0xd0, v as i8 as u8]);
                } else if v >= i16::MIN as i64 {
                    out.push(0xd1);
                    out.extend_from_slice(&(v as i16).to_be_bytes());
                } else if v >= i32::MIN as i64 {
                    out.push(0xd2);
                    out.extend_from_slice(&(v as i32).to_be_bytes());
                } else {
                    out.push(0xd3);
                    out.extend_from_slice(&v.to_be_bytes());
                }
            }
            Value::F32(v) => {
                out.push(0xca);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Value::F64(v) => {
                out.push(0xcb);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Value::Str(s) => {
                let n = s.len();
                if n < 32 {
                    out.push(0xa0 | n as u8);
                } else if n < 0x100 {
                    out.extend_from_slice(&[0xd9, n as u8]);
                } else if n < 0x10000 {
                    out.push(0xda);
                    out.extend_from_slice(&(n as u16).to_be_bytes());
                } else {
                    out.push(0xdb);
                    out.extend_from_slice(&(n as u32).to_be_bytes());
                }
                out.extend_from_slice(s.as_bytes());
            }
            Value::Array(items) => {
                write_len(items.len(), 0x90, 0xdc, 0xdd, out);
                for v in items {
                    v.write(out);
                }
            }
            Value::Map(map) => {
                write_len(map.len(), 0x80, 0xde, 0xdf, out);
                for (k, v) in map {
                    Value::Str(k.clone()).write(out);
                    v.write(out);
                }
            }
        }
    }

    pub fn decode(data: &[u8]) -> Result<Value, Error> {
        let mut pos = 0;
        read(data, &mut pos)
    }

    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Value::UInt(v) => Some(v),
            Value::Int(v) => u64::try_from(v).ok(),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            Value::Bool(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
}

fn write_uint(v: u64, out: &mut Vec<u8>) {
    if v < 0x80 {
        out.push(v as u8);
    } else if v < 0x100 {
        out.extend_from_slice(&[0xcc, v as u8]);
    } else if v < 0x10000 {
        out.push(0xcd);
        out.extend_from_slice(&(v as u16).to_be_bytes());
    } else if v < 0x1_0000_0000 {
        out.push(0xce);
        out.extend_from_slice(&(v as u32).to_be_bytes());
    } else {
        out.push(0xcf);
        out.extend_from_slice(&v.to_be_bytes());
    }
}

fn write_len(n: usize, fix: u8, m16: u8, m32: u8, out: &mut Vec<u8>) {
    if n < 16 {
        out.push(fix | n as u8);
    } else if n < 0x10000 {
        out.push(m16);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(m32);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    }
}

fn take<'a>(data: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8], Error> {
    let s = data.get(*pos..*pos + n).ok_or(Error::Eof)?;
    *pos += n;
    Ok(s)
}

fn be<const N: usize>(data: &[u8], pos: &mut usize) -> Result<[u8; N], Error> {
    Ok(take(data, pos, N)?.try_into().unwrap())
}

fn read(data: &[u8], pos: &mut usize) -> Result<Value, Error> {
    let t = *data.get(*pos).ok_or(Error::Eof)?;
    *pos += 1;
    let str_of = |data: &[u8], pos: &mut usize, n: usize| -> Result<Value, Error> {
        Ok(Value::Str(String::from_utf8_lossy(take(data, pos, n)?).into_owned()))
    };
    Ok(match t {
        0x00..=0x7f => Value::UInt(t as u64),
        0xe0..=0xff => Value::Int(t as i8 as i64),
        0xa0..=0xbf => str_of(data, pos, (t & 0x1f) as usize)?,
        0x90..=0x9f => array(data, pos, (t & 0x0f) as usize)?,
        0x80..=0x8f => map(data, pos, (t & 0x0f) as usize)?,
        0xc0 => Value::Nil,
        0xc2 => Value::Bool(false),
        0xc3 => Value::Bool(true),
        0xcc => Value::UInt(be::<1>(data, pos)?[0] as u64),
        0xcd => Value::UInt(u16::from_be_bytes(be(data, pos)?) as u64),
        0xce => Value::UInt(u32::from_be_bytes(be(data, pos)?) as u64),
        0xcf => Value::UInt(u64::from_be_bytes(be(data, pos)?)),
        0xd0 => Value::Int(be::<1>(data, pos)?[0] as i8 as i64),
        0xd1 => Value::Int(i16::from_be_bytes(be(data, pos)?) as i64),
        0xd2 => Value::Int(i32::from_be_bytes(be(data, pos)?) as i64),
        0xd3 => Value::Int(i64::from_be_bytes(be(data, pos)?)),
        0xca => Value::F32(f32::from_be_bytes(be(data, pos)?)),
        0xcb => Value::F64(f64::from_be_bytes(be(data, pos)?)),
        0xd9 => {
            let n = be::<1>(data, pos)?[0] as usize;
            str_of(data, pos, n)?
        }
        0xda => {
            let n = u16::from_be_bytes(be(data, pos)?) as usize;
            str_of(data, pos, n)?
        }
        0xdb => {
            let n = u32::from_be_bytes(be(data, pos)?) as usize;
            str_of(data, pos, n)?
        }
        0xdc => {
            let n = u16::from_be_bytes(be(data, pos)?) as usize;
            array(data, pos, n)?
        }
        0xdd => {
            let n = u32::from_be_bytes(be(data, pos)?) as usize;
            array(data, pos, n)?
        }
        0xde => {
            let n = u16::from_be_bytes(be(data, pos)?) as usize;
            map(data, pos, n)?
        }
        0xdf => {
            let n = u32::from_be_bytes(be(data, pos)?) as usize;
            map(data, pos, n)?
        }
        t => return Err(Error::Type(t)),
    })
}

fn array(data: &[u8], pos: &mut usize, n: usize) -> Result<Value, Error> {
    (0..n)
        .map(|_| read(data, pos))
        .collect::<Result<_, _>>()
        .map(Value::Array)
}

fn map(data: &[u8], pos: &mut usize, n: usize) -> Result<Value, Error> {
    let mut m = BTreeMap::new();
    for _ in 0..n {
        let k = match read(data, pos)? {
            Value::Str(s) => s,
            other => format!("{other:?}"),
        };
        m.insert(k, read(data, pos)?);
    }
    Ok(Value::Map(m))
}

/// Builds a string-keyed map.
#[macro_export]
macro_rules! mp_map {
    ($($k:expr => $v:expr),* $(,)?) => {{
        let mut m = std::collections::BTreeMap::new();
        $( m.insert(String::from($k), $crate::msgpack::Value::from($v)); )*
        $crate::msgpack::Value::Map(m)
    }};
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}
impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::Int(v as i64)
    }
}
impl From<u32> for Value {
    fn from(v: u32) -> Self {
        Value::UInt(v as u64)
    }
}
impl From<u64> for Value {
    fn from(v: u64) -> Self {
        Value::UInt(v)
    }
}
impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Value::F32(v)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::F64(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Str(v.into())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Str(v)
    }
}
impl From<Vec<Value>> for Value {
    fn from(v: Vec<Value>) -> Self {
        Value::Array(v)
    }
}
impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Value::Nil, Into::into)
    }
}

/// A byte vector as an array of integers (not msgpack bin).
pub fn bytes(data: &[u8]) -> Value {
    Value::Array(data.iter().map(|&b| Value::UInt(b as u64)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let v = mp_map! {
            "b" => vec![Value::Nil, true.into(), (-5i32).into(), (300u32).into(), 1.5f32.into()],
            "a" => "x",
        };
        let bytes = v.encode();
        assert_eq!(&bytes[..3], &[0x82, 0xa1, b'a']);
        assert_eq!(Value::decode(&bytes).unwrap(), v);
    }

    #[test]
    fn integer_forms() {
        assert_eq!(Value::UInt(0x7f).encode(), [0x7f]);
        assert_eq!(Value::UInt(0x80).encode(), [0xcc, 0x80]);
        assert_eq!(Value::Int(-1).encode(), [0xff]);
        assert_eq!(Value::Int(-33).encode(), [0xd0, 0xdf]);
        assert_eq!(Value::UInt(70000).encode(), [0xce, 0, 1, 0x11, 0x70]);
    }
}
