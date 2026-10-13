//! Bulk pipe on interface 0.

use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nusb::MaybeFuture;
use nusb::transfer::{Buffer, Bulk, ControlIn, ControlOut, ControlType, In, Out, Recipient, TransferError};

const EP_OUT: u8 = 0x01;
const EP_IN: u8 = 0x81;
const MAX_TRANSFER: usize = 1 << 20;
/// Reads kept queued at once while a large packet comes in, so the next is ready the moment one completes. One on
/// Windows: with several reads queued there (WinUSB in raw mode), the camera stopped sending a second or so into
/// streaming video and then reset.
#[cfg(not(windows))]
const READ_DEPTH: usize = 4;
#[cfg(windows)]
const READ_DEPTH: usize = 1;
/// Windows starts the interface's driver a moment after the device appears, so opening is retried for this long
/// there. Well under the camera's 10 s watchdog, which resets it if nobody connects.
const OPEN_WAIT: Duration = Duration::from_secs(5);
const CONTROL_WAIT: Duration = Duration::from_secs(1);
const GET_STATUS: u8 = 0;
const CLEAR_FEATURE: u8 = 1;
const U1_ENABLE: u16 = 48;
const U2_ENABLE: u16 = 49;

pub struct Reader {
    ep: nusb::Endpoint<Bulk, In>,
    carry: VecDeque<u8>,
    /// Bytes asked for by reads still queued.
    queued: usize,
    stop: Arc<AtomicBool>,
}

pub struct Writer {
    ep: nusb::Endpoint<Bulk, Out>,
    stop: Arc<AtomicBool>,
}

/// How often a transfer with no time limit checks whether the link was closed.
const STOP_CHECK: Duration = Duration::from_secs(1);

/// Waits for the next transfer to finish, for at most `limit` (`None` on timeout), giving up once `stop` is set.
fn wait_unless_stopped<D>(
    ep: &mut nusb::Endpoint<Bulk, D>,
    stop: &AtomicBool,
    limit: Option<Duration>,
) -> io::Result<Option<nusb::transfer::Completion>>
where
    D: nusb::transfer::EndpointDirection,
{
    let end = limit.map(|l| std::time::Instant::now() + l);
    loop {
        let step = end.map_or(STOP_CHECK, |e| {
            e.saturating_duration_since(std::time::Instant::now()).min(STOP_CHECK)
        });
        if let Some(c) = ep.wait_next_complete(step) {
            return Ok(Some(c));
        }
        if stop.load(Ordering::Relaxed) {
            ep.cancel_all();
            return Err(io::Error::other("link closed"));
        }
        if end.is_some_and(|e| std::time::Instant::now() >= e) {
            return Ok(None);
        }
    }
}

fn claim(info: &nusb::DeviceInfo) -> io::Result<(nusb::Device, nusb::Interface)> {
    let start = std::time::Instant::now();
    loop {
        let result = info
            .open()
            .wait()
            .and_then(|d| Ok((d.detach_and_claim_interface(0).wait()?, d)));
        match result {
            Ok((iface, device)) => return Ok((device, iface)),
            Err(_) if cfg!(windows) && start.elapsed() < OPEN_WAIT => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(e.into()),
        }
    }
}

/// A control request on the default pipe. Windows sends these only through a claimed interface.
fn control_out(device: &nusb::Device, iface: &nusb::Interface, req: ControlOut) -> Result<(), TransferError> {
    #[cfg(windows)]
    let _ = device;
    #[cfg(windows)]
    return iface.control_out(req, CONTROL_WAIT).wait();
    #[cfg(not(windows))]
    {
        let _ = iface;
        device.control_out(req, CONTROL_WAIT).wait()
    }
}

fn control_in(device: &nusb::Device, iface: &nusb::Interface, req: ControlIn) -> Result<Vec<u8>, TransferError> {
    #[cfg(windows)]
    let _ = device;
    #[cfg(windows)]
    return iface.control_in(req, CONTROL_WAIT).wait();
    #[cfg(not(windows))]
    {
        let _ = iface;
        device.control_in(req, CONTROL_WAIT).wait()
    }
}

/// Claims the interface. Transfers with no time limit give up once `stop` is set.
pub fn open(info: &nusb::DeviceInfo, stop: Arc<AtomicBool>) -> io::Result<(Reader, Writer)> {
    let (device, iface) = claim(info)?;
    // With the USB 3 link allowed into its low power states, the camera stopped sending after a few minutes
    // without video. Keep the link awake; best effort, as not every host passes these on, and the bootloader
    // refuses them. The check below says if the application left them on.
    for feature in [U1_ENABLE, U2_ENABLE] {
        let req = ControlOut {
            control_type: ControlType::Standard,
            recipient: Recipient::Device,
            request: CLEAR_FEATURE,
            value: feature,
            index: 0,
            data: &[],
        };
        let _ = control_out(&device, &iface, req);
    }
    // Device status bits 2 and 3: U1 and U2 allowed.
    let status = ControlIn {
        control_type: ControlType::Standard,
        recipient: Recipient::Device,
        request: GET_STATUS,
        value: 0,
        index: 0,
        length: 2,
    };
    match control_in(&device, &iface, status).as_deref() {
        Ok([bits, ..]) if bits & 0b1100 != 0 => {
            eprintln!("usb: link power states still allowed (status {bits:#04x}); video may stop")
        }
        Ok(_) => {}
        Err(e) => eprintln!("usb: could not read link power states: {e}"),
    }
    let ep_in = iface.endpoint::<Bulk, In>(EP_IN)?;
    let ep_out = iface.endpoint::<Bulk, Out>(EP_OUT)?;
    Ok((
        Reader {
            ep: ep_in,
            carry: VecDeque::new(),
            queued: 0,
            stop: stop.clone(),
        },
        Writer { ep: ep_out, stop },
    ))
}

impl Reader {
    /// Reads exactly `n` bytes. `None` waits forever.
    pub fn read_exact(&mut self, n: usize, timeout: Option<Duration>) -> io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            if !self.carry.is_empty() {
                let k = (n - out.len()).min(self.carry.len());
                out.extend(self.carry.drain(..k));
                continue;
            }
            // Queue reads for the rest of the packet, never more than it needs, so none waits on data not yet sent.
            let mps = self.ep.max_packet_size();
            let need = n - out.len();
            while self.ep.pending() < READ_DEPTH && self.queued < need {
                let len = (need - self.queued).min(MAX_TRANSFER).div_ceil(mps) * mps;
                self.ep.submit(Buffer::new(len));
                self.queued += len;
            }
            let done = match timeout {
                Some(t) => match wait_unless_stopped(&mut self.ep, &self.stop, Some(t))? {
                    Some(c) => c,
                    None => {
                        self.cancel();
                        if self.carry.is_empty() {
                            // Kept for the next read, so a partly read header is not lost.
                            self.carry.extend(out);
                            return Err(io::ErrorKind::TimedOut.into());
                        }
                        continue;
                    }
                },
                None => wait_unless_stopped(&mut self.ep, &self.stop, None)?.ok_or(io::ErrorKind::TimedOut)?,
            };
            self.queued = self.queued.saturating_sub(done.buffer.requested_len());
            done.status.map_err(io::Error::other)?;
            let got = &done.buffer[..];
            let k = (n - out.len()).min(got.len());
            out.extend_from_slice(&got[..k]);
            self.carry.extend(&got[k..]);
        }
        Ok(out)
    }

    /// Cancels the queued reads, keeping whatever data they already brought in.
    fn cancel(&mut self) {
        self.ep.cancel_all();
        while self.ep.pending() > 0 {
            if let Some(c) = self.ep.wait_next_complete(Duration::from_secs(1)) {
                self.carry.extend(&c.buffer[..]);
            }
        }
        self.queued = 0;
    }
}

impl Writer {
    pub fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        for chunk in data.chunks(MAX_TRANSFER) {
            self.ep.submit(Buffer::from(chunk.to_vec()));
            let done = wait_unless_stopped(&mut self.ep, &self.stop, None)?.ok_or(io::ErrorKind::TimedOut)?;
            done.status.map_err(io::Error::other)?;
            if done.actual_len != chunk.len() {
                return Err(io::Error::other("short bulk write"));
            }
        }
        Ok(())
    }
}
