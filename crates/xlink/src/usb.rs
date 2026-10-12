//! Bulk pipe on interface 0.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use nusb::MaybeFuture;
use nusb::transfer::{Buffer, Bulk, ControlOut, ControlType, In, Out, Recipient};

const EP_OUT: u8 = 0x01;
const EP_IN: u8 = 0x81;
const MAX_TRANSFER: usize = 1 << 20;
/// Reads kept queued at once while a large packet comes in, so the next is ready the moment one completes.
const READ_DEPTH: usize = 4;
const CLEAR_FEATURE: u8 = 1;
const U1_ENABLE: u16 = 48;
const U2_ENABLE: u16 = 49;

pub struct Reader {
    ep: nusb::Endpoint<Bulk, In>,
    carry: VecDeque<u8>,
    /// Bytes asked for by reads still queued.
    queued: usize,
}

pub struct Writer {
    ep: nusb::Endpoint<Bulk, Out>,
}

pub fn open(info: &nusb::DeviceInfo) -> io::Result<(Reader, Writer)> {
    let device = info.open().wait()?;
    let iface = device.detach_and_claim_interface(0).wait()?;
    // With the USB 3 link allowed into its low power states, the camera stopped sending after a few minutes
    // without video. Keep the link awake; best effort, as not every host passes these on.
    for feature in [U1_ENABLE, U2_ENABLE] {
        let req = ControlOut {
            control_type: ControlType::Standard,
            recipient: Recipient::Device,
            request: CLEAR_FEATURE,
            value: feature,
            index: 0,
            data: &[],
        };
        #[cfg(not(windows))]
        let _ = device.control_out(req, Duration::from_secs(1)).wait();
        #[cfg(windows)]
        let _ = iface.control_out(req, Duration::from_secs(1)).wait();
    }
    let ep_in = iface.endpoint::<Bulk, In>(EP_IN)?;
    let ep_out = iface.endpoint::<Bulk, Out>(EP_OUT)?;
    Ok((
        Reader {
            ep: ep_in,
            carry: VecDeque::new(),
            queued: 0,
        },
        Writer { ep: ep_out },
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
                Some(t) => match self.ep.wait_next_complete(t) {
                    Some(c) => c,
                    None => {
                        self.cancel();
                        if self.carry.is_empty() {
                            return Err(io::ErrorKind::TimedOut.into());
                        }
                        continue;
                    }
                },
                None => loop {
                    if let Some(c) = self.ep.wait_next_complete(Duration::from_secs(3600)) {
                        break c;
                    }
                },
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

    /// Clears a stall on the receive pipe. Needs no reads queued: call after a timed out read.
    pub fn clear_halt(&mut self) -> io::Result<()> {
        self.ep.clear_halt().wait().map_err(io::Error::other)
    }
}

impl Writer {
    pub fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        for chunk in data.chunks(MAX_TRANSFER) {
            self.ep.submit(Buffer::from(chunk.to_vec()));
            let done = loop {
                if let Some(c) = self.ep.wait_next_complete(Duration::from_secs(3600)) {
                    break c;
                }
            };
            done.status.map_err(io::Error::other)?;
            if done.actual_len != chunk.len() {
                return Err(io::Error::other("short bulk write"));
            }
        }
        Ok(())
    }
}
