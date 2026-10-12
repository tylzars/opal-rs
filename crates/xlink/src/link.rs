//! XLink event protocol over the bulk pipe.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::usb::{self, Reader, Writer};

const HEADER_LEN: usize = 84;
const NAME_LEN: usize = 64;
const MAX_PACKETS: u32 = 64;

const WRITE_REQ: u32 = 0;
const READ_REL_REQ: u32 = 2;
const CREATE_STREAM_REQ: u32 = 3;
const CLOSE_STREAM_REQ: u32 = 4;
const PING_REQ: u32 = 5;
const RESET_REQ: u32 = 6;
const RESP: u32 = 8;

const ACK: u32 = 0x1;
const NACK: u32 = 0x2;

#[derive(Clone, Debug, Default)]
struct Header {
    id: i32,
    kind: u32,
    name: String,
    stream: u32,
    size: u32,
    flags: u32,
}

impl Header {
    fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..4].copy_from_slice(&self.id.to_le_bytes());
        b[4..8].copy_from_slice(&self.kind.to_le_bytes());
        let name = self.name.as_bytes();
        b[8..8 + name.len().min(NAME_LEN - 1)].copy_from_slice(&name[..name.len().min(NAME_LEN - 1)]);
        b[72..76].copy_from_slice(&self.stream.to_le_bytes());
        b[76..80].copy_from_slice(&self.size.to_le_bytes());
        b[80..84].copy_from_slice(&self.flags.to_le_bytes());
        b
    }

    fn decode(b: &[u8]) -> Self {
        let u = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let raw = &b[8..8 + NAME_LEN];
        let end = raw.iter().position(|&c| c == 0).unwrap_or(NAME_LEN);
        Self {
            id: u(0) as i32,
            kind: u(4),
            name: String::from_utf8_lossy(&raw[..end]).into_owned(),
            stream: u(72),
            size: u(76),
            flags: u(80),
        }
    }
}

#[derive(Default)]
struct Stream {
    name: String,
    write_size: u32,
    /// Bytes and packets sent but not yet released by the device.
    remote_fill: u32,
    remote_packets: u32,
    incoming: VecDeque<Vec<u8>>,
}

#[derive(Default)]
struct State {
    streams: HashMap<u32, Stream>,
    next_stream: u32,
    responses: HashMap<i32, Header>,
    closed: Option<String>,
}

struct Shared {
    state: Mutex<State>,
    cond: Condvar,
    /// Header and payload for the writer thread. The reader must never wait on USB writes, or the device can stall
    /// sending to us while we stall sending to it.
    out: mpsc::Sender<([u8; HEADER_LEN], Vec<u8>)>,
}

fn write_loop(shared: Arc<Shared>, mut writer: Writer, rx: mpsc::Receiver<([u8; HEADER_LEN], Vec<u8>)>) {
    loop {
        let (header, payload) = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(m) => m,
            Err(mpsc::RecvTimeoutError::Timeout) if shared.state.lock().unwrap().closed.is_none() => continue,
            Err(_) => break,
        };
        let result = writer.write_all(&header).and_then(|_| {
            if payload.is_empty() {
                Ok(())
            } else {
                writer.write_all(&payload)
            }
        });
        if let Err(e) = result {
            let mut st = shared.state.lock().unwrap();
            st.closed.get_or_insert_with(|| e.to_string());
            drop(st);
            shared.cond.notify_all();
            break;
        }
    }
}

static NEXT_ID: AtomicI32 = AtomicI32::new(0xa);

fn next_id() -> i32 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed) + 1
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("link closed: {0}")]
    Closed(String),
    #[error("device refused {0}")]
    Refused(&'static str),
    #[error("timed out")]
    Timeout,
}

/// A connected XLink session.
pub struct Link {
    shared: Arc<Shared>,
}

/// Handle to an open stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(u32);

impl Link {
    /// Claims interface 0, starts the reader and pings the device.
    pub fn connect(info: &nusb::DeviceInfo, timeout: Duration) -> Result<Self, Error> {
        let (reader, writer) = usb::open(info)?;
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            cond: Condvar::new(),
            out: tx,
        });
        let s = shared.clone();
        thread::Builder::new()
            .name("xlink-reader".into())
            .spawn(move || read_loop(s, reader))
            .map_err(io::Error::other)?;
        let s = shared.clone();
        thread::Builder::new()
            .name("xlink-writer".into())
            .spawn(move || write_loop(s, writer, rx))
            .map_err(io::Error::other)?;
        let link = Self { shared };
        let ping = Header {
            id: next_id(),
            kind: PING_REQ,
            flags: ACK,
            ..Default::default()
        };
        link.request(ping, &[], Some(timeout))?;
        Ok(link)
    }

    fn send(&self, h: &Header, payload: &[u8]) -> Result<(), Error> {
        self.shared
            .out
            .send((h.encode(), payload.to_vec()))
            .map_err(|_| Error::Closed("writer stopped".into()))
    }

    fn request(&self, h: Header, payload: &[u8], timeout: Option<Duration>) -> Result<Header, Error> {
        let id = h.id;
        self.send(&h, payload)?;
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut st = self.shared.state.lock().unwrap();
        loop {
            if let Some(r) = st.responses.remove(&id) {
                return Ok(r);
            }
            if let Some(why) = &st.closed {
                return Err(Error::Closed(why.clone()));
            }
            st = wait(&self.shared.cond, st, deadline)?;
        }
    }

    /// Opens a host-to-device stream with room for `write_size` bytes in flight.
    pub fn open_stream(&self, name: &str, write_size: u32) -> Result<StreamId, Error> {
        let write_size = write_size.div_ceil(64) * 64;
        let id = {
            let mut st = self.shared.state.lock().unwrap();
            let id = stream_for(&mut st, name);
            let s = st.streams.get_mut(&id).unwrap();
            s.write_size = s.write_size.max(write_size);
            id
        };
        let h = Header {
            id: next_id(),
            kind: CREATE_STREAM_REQ,
            name: name.into(),
            stream: id,
            size: write_size,
            flags: ACK,
        };
        let r = self.request(h, &[], Some(Duration::from_secs(5)))?;
        if r.flags & ACK == 0 {
            return Err(Error::Refused("stream"));
        }
        Ok(StreamId(id))
    }

    /// Sends one packet. Blocks until the device has room for it and acknowledges it.
    pub fn write(&self, stream: StreamId, data: &[u8]) -> Result<(), Error> {
        let n = data.len() as u32;
        {
            let mut st = self.shared.state.lock().unwrap();
            loop {
                if let Some(why) = &st.closed {
                    return Err(Error::Closed(why.clone()));
                }
                let s = st.streams.get(&stream.0).ok_or(Error::Refused("unknown stream"))?;
                if s.remote_packets < MAX_PACKETS && s.remote_fill + n <= s.write_size {
                    break;
                }
                st = wait(&self.shared.cond, st, None)?;
            }
            let s = st.streams.get_mut(&stream.0).unwrap();
            s.remote_fill += n;
            s.remote_packets += 1;
        }
        let h = Header {
            id: next_id(),
            kind: WRITE_REQ,
            stream: stream.0,
            size: n,
            flags: ACK,
            ..Default::default()
        };
        let r = self.request(h, data, None)?;
        if r.flags & ACK == 0 {
            return Err(Error::Refused("write"));
        }
        Ok(())
    }

    /// Sends `data` as consecutive packets of at most `split` bytes.
    pub fn write_split(&self, stream: StreamId, data: &[u8], split: usize) -> Result<(), Error> {
        for chunk in data.chunks(split) {
            self.write(stream, chunk)?;
        }
        Ok(())
    }

    /// Takes the next packet the device wrote on a stream and releases it back to the device.
    pub fn read(&self, name: &str, timeout: Option<Duration>) -> Result<Vec<u8>, Error> {
        let deadline = timeout.map(|t| Instant::now() + t);
        let (stream, packet) = {
            let mut st = self.shared.state.lock().unwrap();
            loop {
                let id = stream_for(&mut st, name);
                if let Some(p) = st.streams.get_mut(&id).unwrap().incoming.pop_front() {
                    break (id, p);
                }
                if let Some(why) = &st.closed {
                    return Err(Error::Closed(why.clone()));
                }
                st = wait(&self.shared.cond, st, deadline)?;
            }
        };
        let h = Header {
            id: next_id(),
            kind: READ_REL_REQ,
            stream,
            size: packet.len() as u32,
            flags: ACK,
            ..Default::default()
        };
        self.request(h, &[], None)?;
        Ok(packet)
    }

    /// Waits until the reader sees the link go down.
    pub fn wait_closed(&self, timeout: Duration) -> bool {
        let deadline = Some(Instant::now() + timeout);
        let mut st = self.shared.state.lock().unwrap();
        while st.closed.is_none() {
            match wait(&self.shared.cond, st, deadline) {
                Ok(g) => st = g,
                Err(_) => return false,
            }
        }
        true
    }
}

fn stream_for(st: &mut State, name: &str) -> u32 {
    if let Some((&id, _)) = st.streams.iter().find(|(_, s)| s.name == name) {
        return id;
    }
    let id = st.next_stream;
    st.next_stream += 1;
    st.streams.insert(
        id,
        Stream {
            name: name.into(),
            ..Default::default()
        },
    );
    id
}

fn wait<'a>(
    cond: &Condvar,
    guard: std::sync::MutexGuard<'a, State>,
    deadline: Option<Instant>,
) -> Result<std::sync::MutexGuard<'a, State>, Error> {
    match deadline {
        None => Ok(cond.wait(guard).unwrap()),
        Some(d) => {
            let now = Instant::now();
            if now >= d {
                return Err(Error::Timeout);
            }
            Ok(cond.wait_timeout(guard, d - now).unwrap().0)
        }
    }
}

fn reply(shared: &Shared, req: &Header, stream: u32, size: u32, name: &str, flags: u32) {
    let h = Header {
        id: req.id,
        kind: req.kind + RESP,
        name: name.into(),
        stream,
        size,
        flags,
    };
    let _ = shared.out.send((h.encode(), Vec::new()));
}

fn read_loop(shared: Arc<Shared>, mut reader: Reader) {
    let why = loop {
        let h = match reader.read_exact(HEADER_LEN, None) {
            Ok(b) => Header::decode(&b),
            Err(e) => break e.to_string(),
        };
        match h.kind {
            WRITE_REQ => {
                let data = match reader.read_exact(h.size as usize, None) {
                    Ok(d) => d,
                    Err(e) => break e.to_string(),
                };
                {
                    let mut st = shared.state.lock().unwrap();
                    st.streams.entry(h.stream).or_default().incoming.push_back(data);
                }
                shared.cond.notify_all();
                reply(&shared, &h, h.stream, h.size, "", ACK);
            }
            READ_REL_REQ => {
                {
                    let mut st = shared.state.lock().unwrap();
                    if let Some(s) = st.streams.get_mut(&h.stream) {
                        s.remote_fill = s.remote_fill.saturating_sub(h.size);
                        s.remote_packets = s.remote_packets.saturating_sub(1);
                    }
                }
                shared.cond.notify_all();
                reply(&shared, &h, h.stream, 0, "", ACK);
            }
            CREATE_STREAM_REQ => {
                let id = stream_for(&mut shared.state.lock().unwrap(), &h.name);
                reply(&shared, &h, id, h.size, &h.name, ACK);
            }
            CLOSE_STREAM_REQ => {
                let pending = {
                    let st = shared.state.lock().unwrap();
                    st.streams.get(&h.stream).is_some_and(|s| !s.incoming.is_empty())
                };
                reply(&shared, &h, h.stream, 0, "", if pending { NACK } else { ACK });
            }
            PING_REQ => reply(&shared, &h, 0, 0, "", ACK),
            RESET_REQ => {
                reply(&shared, &h, 0, 0, "", ACK);
                break "device reset".into();
            }
            k if (RESP..RESP + 7).contains(&k) => {
                shared.state.lock().unwrap().responses.insert(h.id, h);
                shared.cond.notify_all();
            }
            k => break format!("unexpected event type {k}"),
        }
    };
    shared.state.lock().unwrap().closed = Some(why);
    shared.cond.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_stream_header_bytes() {
        let h = Header {
            id: 12,
            kind: CREATE_STREAM_REQ,
            name: "__bootloader".into(),
            stream: 0,
            size: 5 << 20,
            flags: ACK,
        };
        let b = h.encode();
        assert_eq!(&b[0..8], &[0x0c, 0, 0, 0, 3, 0, 0, 0]);
        assert_eq!(&b[8..20], b"__bootloader");
        assert!(b[20..72].iter().all(|&x| x == 0));
        assert_eq!(&b[72..84], &[0, 0, 0, 0, 0, 0, 0x50, 0, 1, 0, 0, 0]);
        let d = Header::decode(&b);
        assert_eq!(
            (d.id, d.kind, d.name.as_str(), d.size),
            (12, 3, "__bootloader", 5 << 20)
        );
    }
}
