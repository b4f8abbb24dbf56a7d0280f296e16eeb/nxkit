//! `NvOsDrv*` take-overs.
//!
//! The host takes over NVN entirely and has no `nvdrv` service. Only the hardware decoder of the SDK movie library
//! opens `/dev/nvhost-*` directly. Opening a device fails, and every ioctl, mapping and event query returns an error,
//! so the decoder takes its own failure path.

use super::{get_cstr, log_once_with, nothing};
use std::sync::atomic::AtomicBool;

/// `NvError` 3, not initialized. Non-zero means failure.
const NV_ERR: u32 = 3;

static OPEN_NOTE: AtomicBool = AtomicBool::new(false);

/// `NvOsDrvOpen(const char*)`: no device opens.
extern "C" fn open(path: u64) -> i32 {
    log_once_with(&OPEN_NOTE, || {
        let path = if path != 0 {
            get_cstr(path, 256)
        } else {
            "(null)".to_owned()
        };
        format!(
            "nvdrv: cannot open {path} (the host has no nvdrv, only hardware movie decoding uses it)"
        )
    });
    -1
}

/// Every call of the driver that needs a device.
extern "C" fn error() -> u32 {
    NV_ERR
}

table! {
    "NvOsDrvOpen" => open,
    "NvOsDrvClose" => nothing,
    "NvOsDrvIoctl" => error,
    "NvOsDrvIoctl2" => error,
    "NvOsDrvIoctl3" => error,
    "NvOsDrvMapSharedMem" => error,
    "_Z17NvOsDrvQueryEventijPN2nn2os15SystemEventTypeE" => error,
}
