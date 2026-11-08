//! `audout:u` and `IAudioOut` over IPC, on the same mixer as the SDK take-overs of AudioOut.
//!
//! This is the fallback for SDK code that does not go through the take-overs of `sdk_out`. The buffers stay in guest
//! memory and the mixer reads them in the output callback.

use crate::engine::{Buf, Notify, RATE, mixer};
use crate::util::{RES_FAIL, put_name};
use nxkit_abi::guest;
use nxkit_abi::ipc::{BufferKind, Handled, Request, Response, Service};
use nxkit_abi::kernel;
use std::sync::Arc;

const NAME_MAX: usize = 0x100;
const USER_BUFFER: usize = 0x28;

/// The `audout:u` service.
pub(crate) struct AudioOutManager;

impl AudioOutManager {
    fn list(req: &Request<'_>, rsp: &mut Response) {
        if let Some(b) = req.buffer(BufferKind::Receive, 0) {
            if b.size != 0 {
                put_name(b.addr, "DeviceOut", (b.size as usize).min(NAME_MAX));
            }
        }
        rsp.push_u32(1);
    }

    fn open(req: &Request<'_>, rsp: &mut Response) {
        let channels = match req.u32_at(4).map_or(0, |v| v & 0xffff) {
            c @ (1 | 2 | 6) => c,
            _ => 2,
        };
        let Some(k) = kernel::kernel() else {
            eprintln!("nxkit-audio: [RED] audout: no kernel, cannot open an output");
            rsp.result = RES_FAIL;
            return;
        };
        let Ok((writable, readable)) = k.create_event() else {
            eprintln!("nxkit-audio: [RED] audout: the buffer event could not be created");
            rsp.result = RES_FAIL;
            return;
        };
        let Some(idx) = mixer().open(channels, Notify::Kernel(writable)) else {
            eprintln!("nxkit-audio: [RED] audout: all outputs are in use");
            let _ = k.close_handle(writable);
            let _ = k.close_handle(readable);
            rsp.result = RES_FAIL;
            return;
        };
        if let Some(b) = req.buffer(BufferKind::Receive, 0) {
            if b.size != 0 {
                put_name(b.addr, "DeviceOut", (b.size as usize).min(NAME_MAX));
            }
        }
        rsp.push_u32(RATE);
        rsp.push_u32(channels);
        // The sample format is PCM16.
        rsp.push_u32(2);
        rsp.push_u32(mixer().state(idx));
        rsp.objects.push(Arc::new(AudioOutIface {
            idx,
            writable,
            readable,
        }));
    }
}

impl Service for AudioOutManager {
    fn name(&self) -> &'static str {
        "audout:u"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 | 2 => Self::list(req, rsp),
            1 | 3 => Self::open(req, rsp),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// One `IAudioOut`: an output of the mixer.
struct AudioOutIface {
    idx: usize,
    writable: u32,
    readable: u32,
}

impl Drop for AudioOutIface {
    fn drop(&mut self) {
        mixer().close(self.idx);
        if let Some(k) = kernel::kernel() {
            let _ = k.close_handle(self.writable);
            let _ = k.close_handle(self.readable);
        }
    }
}

impl AudioOutIface {
    fn append(&self, req: &Request<'_>, rsp: &mut Response) {
        let Some(tag) = req.u64_at(0) else {
            rsp.result = RES_FAIL;
            return;
        };
        let Some(b) = req.buffer(BufferKind::Send, 0) else {
            eprintln!("nxkit-audio: [RED] audout: AppendAudioOutBuffer without a buffer");
            rsp.result = RES_FAIL;
            return;
        };
        let mut ub = [0u8; USER_BUFFER];
        if b.size < USER_BUFFER as u64 || guest::read(b.addr, &mut ub).is_err() {
            eprintln!(
                "nxkit-audio: [RED] audout: the AudioUserBuffer at {:#x} is not readable",
                b.addr
            );
            rsp.result = RES_FAIL;
            return;
        }
        let le = |o: usize| {
            u64::from_le_bytes([
                ub[o],
                ub[o + 1],
                ub[o + 2],
                ub[o + 3],
                ub[o + 4],
                ub[o + 5],
                ub[o + 6],
                ub[o + 7],
            ])
        };
        let buf = Buf {
            token: tag,
            addr: le(8),
            size: le(0x18),
            host: false,
        };
        if !mixer().append(self.idx, buf) {
            eprintln!("nxkit-audio: [RED] audout: the buffer queue is full, the buffer is dropped");
            rsp.result = RES_FAIL;
        }
    }

    fn released(&self, req: &Request<'_>, rsp: &mut Response) {
        let room = req
            .buffer(BufferKind::Receive, 0)
            .map_or(0, |b| (b.size / 8) as usize);
        let mut tags: Vec<u8> = Vec::new();
        let mut n = 0u32;
        while (n as usize) < room {
            let Some(b) = mixer().released(self.idx) else {
                break;
            };
            tags.extend_from_slice(&b.token.to_le_bytes());
            n += 1;
        }
        if let Some(b) = req.buffer(BufferKind::Receive, 0) {
            if !tags.is_empty() && guest::write(b.addr, &tags).is_err() {
                eprintln!(
                    "nxkit-audio: [RED] audout: the released buffer list at {:#x} is not writable",
                    b.addr
                );
            }
        }
        rsp.push_u32(n);
    }
}

impl Service for AudioOutIface {
    fn name(&self) -> &'static str {
        "IAudioOut"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        let m = mixer();
        match req.command {
            0 => rsp.push_u32(m.state(self.idx)),
            1 => m.start(self.idx),
            2 => m.stop(self.idx),
            3 | 7 => self.append(req, rsp),
            4 => rsp.copy_handles.push(self.readable),
            5 | 8 => self.released(req, rsp),
            6 => {
                let tag = req.u64_at(0).unwrap_or(0);
                rsp.push_u32(u32::from(m.contains(self.idx, tag)));
            }
            9 => rsp.push_u32(m.queued(self.idx)),
            10 => rsp.push_u64(m.played(self.idx)),
            11 => {
                let any = m.queued(self.idx) != 0;
                m.purge(self.idx);
                rsp.push_u32(u32::from(any));
            }
            12 => m.set_volume(
                self.idx,
                f32::from_bits(req.u32_at(0).unwrap_or(1.0f32.to_bits())),
            ),
            13 => rsp.push_u32(m.volume(self.idx).to_bits()),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}
