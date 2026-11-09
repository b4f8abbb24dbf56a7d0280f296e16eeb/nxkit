//! `audren:u` and `IAudioDevice` over IPC.
//!
//! nxnative has no DSP renderer on the host. The SDK renders in the guest (see `sdk_ren`). Over IPC the manager therefore
//! answers the work buffer size and the device service, and `OpenAudioRenderer` is not supported. The host says so
//! loudly (R15) when a game asks.

use crate::ren_work::service_work_size;
use crate::sdk_dev::{AUTOTUNE, DEV_VOL, DEVNAME, SPEAKER};
use crate::util::{RES_FAIL, put_name};
use nxkit_abi::ipc::{BufferKind, Handled, Request, Response, Service};
use nxkit_abi::kernel;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// The audren result code `NotSupported`.
const NOT_SUPPORTED: u32 = 0x40299;

/// The `audren:u` service.
pub(crate) struct AudioRendererManager;

impl AudioRendererManager {
    fn work_size(req: &Request<'_>, rsp: &mut Response) {
        let Some(d) = req.data.get(0..0x34) else {
            rsp.result = RES_FAIL;
            return;
        };
        // The service form keeps the revision at +0x30. `service_work_size` reads the parameter form of the SDK.
        let mut p = [0u8; 0x40];
        p[..0x30].copy_from_slice(&d[..0x30]);
        p[0x34..0x38].copy_from_slice(&d[0x30..0x34]);
        let n = service_work_size(&p);
        if n == 0 {
            eprintln!(
                "nxkit-audio: [RED] audren: GetWorkBufferSize for an unknown revision or an invalid parameter"
            );
            rsp.result = NOT_SUPPORTED;
            return;
        }
        rsp.push_u64(n as u64);
    }

    fn device_service(rsp: &mut Response) {
        let Some(k) = kernel::kernel() else {
            eprintln!("nxkit-audio: [RED] audren: no kernel, cannot make IAudioDevice");
            rsp.result = RES_FAIL;
            return;
        };
        let Ok((writable, readable)) = k.create_event() else {
            eprintln!("nxkit-audio: [RED] audren: the device event could not be created");
            rsp.result = RES_FAIL;
            return;
        };
        rsp.objects
            .push(Arc::new(AudioDevice { writable, readable }));
    }
}

impl Service for AudioRendererManager {
    fn name(&self) -> &'static str {
        "audren:u"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 | 3 => {
                eprintln!(
                    "nxkit-audio: [RED] audren: command {} opens a renderer over IPC, which the host does not provide. The SDK's renderer runs in the guest",
                    req.command
                );
                rsp.result = NOT_SUPPORTED;
            }
            1 => Self::work_size(req, rsp),
            2 | 4 => Self::device_service(rsp),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// `IAudioDevice`: one built-in speaker with two channels.
struct AudioDevice {
    writable: u32,
    readable: u32,
}

impl Drop for AudioDevice {
    fn drop(&mut self) {
        if let Some(k) = kernel::kernel() {
            let _ = k.close_handle(self.writable);
            let _ = k.close_handle(self.readable);
        }
    }
}

fn write_name(req: &Request<'_>) {
    if let Some(b) = req.buffer(BufferKind::Receive, 0)
        && b.size != 0
    {
        put_name(b.addr, SPEAKER, (b.size as usize).min(DEVNAME));
    }
}

impl AudioDevice {
    fn list(req: &Request<'_>, rsp: &mut Response) {
        write_name(req);
        rsp.push_u32(1);
    }

    fn set_volume(req: &Request<'_>) {
        if let Some(bits) = req.u32_at(0) {
            DEV_VOL.store(bits, Ordering::Relaxed);
        }
    }
}

impl Service for AudioDevice {
    fn name(&self) -> &'static str {
        "IAudioDevice"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 | 6 | 14 => Self::list(req, rsp),
            1 | 7 => Self::set_volume(req),
            2 | 8 => rsp.push_u32(DEV_VOL.load(Ordering::Relaxed)),
            3 | 10 | 13 => write_name(req),
            4 | 11 | 12 => rsp.copy_handles.push(self.readable),
            5 => rsp.push_u32(2),
            15..=18 => {
                eprintln!(
                    "nxkit-audio: audren: IAudioDevice command {} (device notification) is not supported",
                    req.command
                );
                rsp.result = NOT_SUPPORTED;
            }
            19 => AUTOTUNE.store(req.u32_at(0).unwrap_or(0) & 1 != 0, Ordering::Relaxed),
            20 => rsp.push_u32(u32::from(AUTOTUNE.load(Ordering::Relaxed))),
            21 => rsp.push_u32(0),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}
