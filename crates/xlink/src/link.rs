//! XLink event protocol over the bulk pipe.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
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
    started: Instant,
    /// When the device last sent anything, in milliseconds after `started`.
    last_heard: AtomicU64,
    /// The last packet headers each way, for working out why a device went quiet.
    trace: Mutex<VecDeque<String>>,
    /// Set while this side is ending the session, when silence is expected.
    ending: AtomicBool,
}

const TRACE_LEN: usize = 16;

fn note(shared: &Shared, dir: &str, h: &Header) {
    let kind = match h.kind % RESP {
        WRITE_REQ => "write",
        READ_REL_REQ => "release",
        CREATE_STREAM_REQ => "create",
        CLOSE_STREAM_REQ => "close",
        PING_REQ => "ping",
        RESET_REQ => "reset",
        _ => "other",
    };
    let line = format!(
        "{:.3} {dir} {kind}{} stream {} size {}",
        shared.started.elapsed().as_secs_f64(),
        if h.kind >= RESP { " reply" } else { "" },
        h.stream,
        h.size
    );
    let mut t = shared.trace.lock().unwrap();
    if t.len() == TRACE_LEN {
        t.pop_front();
    }
    t.push_back(line);
}

/// Recent packets each way and how full each stream is.
fn dump(shared: &Shared) -> String {
    let mut out: Vec<String> = shared.trace.lock().unwrap().iter().cloned().collect();
    let st = shared.state.lock().unwrap();
    let mut ids: Vec<_> = st.streams.keys().copied().collect();
    ids.sort();
    for id in ids {
        let s = &st.streams[&id];
        out.push(format!(
            "stream {id} {}: sent {} packets {} bytes not released, {} unread",
            s.name,
            s.remote_packets,
            s.remote_fill,
            s.incoming.len()
        ));
    }
    out.join("\n")
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
        // Noted once sent, so a trace shows whether writes were still getting through.
        note(&shared, "out", &Header::decode(&header));
        if let Err(e) = result {
            let mut st = shared.state.lock().unwrap();
            st.closed.get_or_insert_with(|| e.to_string());
            drop(st);
            shared.cond.notify_all();
            break;
        }
    }
}

/// How long the reader waits for the device before queueing its reads again.
const QUIET_STEP: Duration = Duration::from_secs(5);

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
            started: Instant::now(),
            last_heard: AtomicU64::new(0),
            trace: Mutex::new(VecDeque::new()),
            ending: AtomicBool::new(false),
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

    /// The last packets each way and how full each stream is, for diagnosis.
    pub fn trace(&self) -> String {
        dump(&self.shared)
    }

    /// Time since the device last sent anything.
    pub fn quiet_for(&self) -> Duration {
        let heard = Duration::from_millis(self.shared.last_heard.load(Ordering::Relaxed));
        self.shared.started.elapsed().saturating_sub(heard)
    }

    /// Tells the link the session is ending, so the device going quiet is not reported.
    pub fn ending(&self) {
        self.shared.ending.store(true, Ordering::Relaxed);
    }

    /// Ends the link from this side, for a device that stopped answering. Everything waiting on it returns.
    pub fn close(&self, why: &str) {
        let mut st = self.shared.state.lock().unwrap();
        if st.closed.is_none() {
            st.closed = Some(why.into());
        }
        drop(st);
        self.shared.cond.notify_all();
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

/// The device closing a stream it writes to. Refused while packets it sent are unread. Once closed, the stream's
/// name is free, so the device can open it again (as it does to make room for bigger packets) and gets a new id.
fn close_remote(st: &mut State, id: u32) -> bool {
    let Some(s) = st.streams.get_mut(&id) else {
        return true;
    };
    if !s.incoming.is_empty() {
        return false;
    }
    if s.write_size == 0 {
        st.streams.remove(&id);
    }
    true
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
    // Seconds of silence so far, counted in steps of QUIET_STEP.
    let mut quiet = 0;
    let why = loop {
        let h = match reader.read_exact(HEADER_LEN, Some(QUIET_STEP)) {
            Ok(b) => {
                if quiet > 0 {
                    eprintln!("xlink: device heard again after {}s", quiet * QUIET_STEP.as_secs());
                    quiet = 0;
                }
                Header::decode(&b)
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                if shared.state.lock().unwrap().closed.is_some() {
                    break "closed".into();
                }
                if shared.ending.load(Ordering::Relaxed) {
                    continue;
                }
                // A device that goes quiet may be stuck sending to us: queue fresh reads, then clear the pipe.
                quiet += 1;
                if quiet == 2 {
                    eprintln!("xlink: last packets and stream state:\n{}", dump(&shared));
                }
                let cleared = if quiet == 4 { Some(reader.clear_halt()) } else { None };
                eprintln!(
                    "xlink: nothing from the device for {}s{}",
                    quiet * QUIET_STEP.as_secs(),
                    match cleared {
                        Some(Ok(())) => ", cleared the receive pipe".to_owned(),
                        Some(Err(e)) => format!(", clearing the receive pipe failed: {e}"),
                        None => String::new(),
                    }
                );
                continue;
            }
            Err(e) => break e.to_string(),
        };
        shared
            .last_heard
            .store(shared.started.elapsed().as_millis() as u64, Ordering::Relaxed);
        note(&shared, "in", &h);
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
                let closed = close_remote(&mut shared.state.lock().unwrap(), h.stream);
                reply(&shared, &h, h.stream, 0, "", if closed { ACK } else { NACK });
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
    shared.state.lock().unwrap().closed.get_or_insert(why);
    shared.cond.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopened_device_stream_gets_a_new_id() {
        let mut st = State::default();
        let rgb = stream_for(&mut st, "rgb");
        let control = stream_for(&mut st, "control");
        st.streams.get_mut(&control).unwrap().write_size = 1024;
        st.streams.get_mut(&rgb).unwrap().incoming.push_back(vec![1]);
        assert!(!close_remote(&mut st, rgb), "unread packets");
        st.streams.get_mut(&rgb).unwrap().incoming.clear();
        assert!(close_remote(&mut st, rgb));
        let again = stream_for(&mut st, "rgb");
        assert_ne!(again, rgb);
        assert_ne!(again, control);
        // Streams the host writes to keep their id.
        assert!(close_remote(&mut st, control));
        assert_eq!(stream_for(&mut st, "control"), control);
        assert!(close_remote(&mut st, 999));
    }

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
