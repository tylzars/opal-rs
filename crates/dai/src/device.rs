//! Session with an application running on the camera: RPC, watchdog, time sync and logs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use opal_xlink::{Found, Link, StreamId};

use crate::msgpack::Value;
use crate::nop;
use crate::nop_struct;

pub const RPC_STREAM: &str = "__rpc_main";
pub const STREAM_SIZE: u32 = 5 << 20;
const WATCHDOG_PERIOD: Duration = Duration::from_millis(750);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Link(#[from] opal_xlink::Error),
    #[error("bad RPC response")]
    BadResponse,
    #[error("device: {0}")]
    Remote(String),
}

/// nanorpc method id: polynomial hash of the name.
pub fn method_id(name: &str) -> u64 {
    name.bytes()
        .fold(1125899906842597u64, |h, c| h.wrapping_mul(31).wrapping_add(c as u64))
}

pub fn encode_request(method: &str, args: Vec<Value>) -> Vec<u8> {
    let args = if args.is_empty() {
        Value::Nil
    } else {
        Value::Array(args)
    };
    Value::Array(vec![
        Value::UInt(1),
        Value::UInt(1),
        Value::UInt(method_id(method)),
        args,
    ])
    .encode()
}

pub fn decode_response(data: &[u8]) -> Result<Value, Error> {
    let v = Value::decode(data).map_err(|_| Error::BadResponse)?;
    let items = v.as_array().ok_or(Error::BadResponse)?;
    if items.len() < 3 || items[0].as_u64() != Some(1) || items[1].as_u64() != Some(2) {
        return Err(Error::BadResponse);
    }
    match items[2].as_u64() {
        Some(1) => Ok(items.get(3).cloned().unwrap_or(Value::Nil)),
        _ => Err(Error::Remote(
            items.get(3).and_then(Value::as_str).unwrap_or("").to_owned(),
        )),
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timestamp {
    pub sec: i64,
    pub nsec: i64,
}
nop_struct!(Timestamp { sec, nsec });

#[derive(Clone, Debug, PartialEq)]
pub struct LogMessage {
    pub node_id_name: String,
    pub level: i32,
    pub time: Timestamp,
    pub color_range_start: u64,
    pub color_range_end: u64,
    pub payload: String,
}
nop_struct!(LogMessage {
    node_id_name,
    level,
    time,
    color_range_start,
    color_range_end,
    payload
});

pub const LOG_WARN: i32 = 3;

#[cfg(unix)]
fn monotonic() -> Timestamp {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    Timestamp {
        sec: ts.tv_sec,
        nsec: ts.tv_nsec,
    }
}

/// A steady clock from when it was first read; the camera only needs it to move forward evenly.
#[cfg(not(unix))]
fn monotonic() -> Timestamp {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let t = START.get_or_init(std::time::Instant::now).elapsed();
    Timestamp {
        sec: t.as_secs() as i64,
        nsec: t.subsec_nanos() as i64,
    }
}

/// A connected camera running the depthai application.
pub struct Device {
    link: Arc<Link>,
    rpc: Mutex<StreamId>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Device {
    /// Connects to a booted camera. `on_log` receives device log lines.
    pub fn connect(found: &Found, on_log: impl Fn(LogMessage) + Send + 'static) -> Result<Self, Error> {
        let link = Arc::new(Link::connect(&found.info, Duration::from_secs(5))?);
        let rpc = link.open_stream(RPC_STREAM, STREAM_SIZE)?;
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();

        let wd = link.open_stream("__watchdog", 128)?;
        threads.push(spawn("watchdog", link.clone(), stop.clone(), move |link| {
            link.write(wd, &[0; 4])?;
            thread::sleep(WATCHDOG_PERIOD);
            Ok(())
        }));

        let ts = link.open_stream("__timesync", 128)?;
        threads.push(spawn("timesync", link.clone(), stop.clone(), move |link| {
            link.read("__timesync", None)?;
            let t = monotonic();
            let mut b = Vec::with_capacity(16);
            b.extend_from_slice(&t.sec.to_le_bytes());
            b.extend_from_slice(&t.nsec.to_le_bytes());
            link.write(ts, &b)
        }));

        link.open_stream("__log", 128)?;
        threads.push(spawn("log", link.clone(), stop.clone(), move |link| {
            let packet = link.read("__log", None)?;
            if let Ok(msgs) = nop::from_slice::<Vec<LogMessage>>(&packet) {
                msgs.into_iter().for_each(&on_log);
            }
            Ok(())
        }));

        let dev = Self {
            link,
            rpc: Mutex::new(rpc),
            stop,
            threads,
        };
        dev.call("setLogLevel", vec![LOG_WARN.into()])?;
        Ok(dev)
    }

    pub fn link(&self) -> &Arc<Link> {
        &self.link
    }

    /// Calls a method on the device and returns its result.
    pub fn call(&self, method: &str, args: Vec<Value>) -> Result<Value, Error> {
        let stream = self.rpc.lock().unwrap();
        self.link.write(*stream, &encode_request(method, args))?;
        let resp = self.link.read(RPC_STREAM, Some(Duration::from_secs(30)))?;
        decode_response(&resp)
    }

    /// Ends the session. The camera restarts on its own once the watchdog stops.
    pub fn close(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        self.link.ending();
        // An XLink reset can leave the camera off the bus, so let its watchdog expire instead.
        if self.link.wait_closed(Duration::from_secs(10)) {
            for t in self.threads.drain(..) {
                let _ = t.join();
            }
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn spawn(
    name: &str,
    link: Arc<Link>,
    stop: Arc<AtomicBool>,
    mut step: impl FnMut(&Link) -> Result<(), opal_xlink::Error> + Send + 'static,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if step(&link).is_err() {
                    break;
                }
            }
        })
        .expect("spawn thread")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    #[test]
    fn request_vectors() {
        assert_eq!(
            encode_request("setLogLevel", vec![3i32.into()]),
            hex("94 01 01 cf 8d 4f 73 89 1d ca a0 1d 91 03")
        );
        assert_eq!(
            encode_request("isPipelineRunning", vec![]),
            hex("94 01 01 cf f1 d4 0c 6a 3d 42 c0 ee c0")
        );
        assert_eq!(
            encode_request("setSystemInformationLoggingRate", vec![1.0f32.into()]),
            hex("94 01 01 cf 50 8d 08 86 9f 47 92 1f 91 ca 3f 80 00 00")
        );
        assert_eq!(
            encode_request("memAlloc", vec![70000u32.into()]),
            hex("94 01 01 cf bc 03 ed de 34 44 b5 45 91 ce 00 01 11 70")
        );
    }

    #[test]
    fn response_vectors() {
        assert_eq!(decode_response(&hex("94 01 02 01 c2")).unwrap(), Value::Bool(false));
        assert_eq!(decode_response(&hex("93 01 02 01")).unwrap(), Value::Nil);
        assert!(matches!(decode_response(&hex("94 01 02 00 a4 62 6f 6f 6d")), Err(Error::Remote(m)) if m == "boom"));
    }

    #[test]
    fn log_vector() {
        let msgs: Vec<LogMessage> =
            nop::from_slice(&hex("ba 01 b9 06 bd 04 68 6f 73 74 03 b9 02 0c 22 00 00 bd 02 68 69")).unwrap();
        assert_eq!(msgs[0].node_id_name, "host");
        assert_eq!(msgs[0].level, LOG_WARN);
        assert_eq!(msgs[0].time, Timestamp { sec: 12, nsec: 34 });
        assert_eq!(msgs[0].payload, "hi");
    }
}
