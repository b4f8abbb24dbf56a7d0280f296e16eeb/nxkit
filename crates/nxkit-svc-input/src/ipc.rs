//! The `hid` IPC service, for the code of the SDK that nxnative does not replace and for games that read the HID
//! shared memory themselves.
//!
//! The command IDs and the data layouts are from memory of the switchbrew pages and unverified. A command that is not
//! listed here stops the game with its ID (R15), so the list grows from what a game asks for.

use std::sync::{Arc, Mutex, OnceLock};

use nxkit_abi::guest;
use nxkit_abi::ipc::{BufferKind, Handled, Named, Request, Response, Service};
use nxkit_abi::kernel;

use crate::npad::id_bit;
use crate::rumble::VibrationValue;
use crate::shmem::{self, SharedMemory};
use crate::with;

/// `ResultInvalidArgument` of hid, used for malformed requests.
const RESULT_INVALID: u32 = 2 | 0x100 << 9;
/// The most ids a `SetSupportedNpadIdType` request may carry.
const MAX_IDS: usize = 64;

/// The root interface, `IHidServer`.
pub(crate) struct HidServer;

/// `IAppletResource`.
pub(crate) struct AppletResource {
    handle: Mutex<Option<u32>>,
}

/// The services this crate registers with the IPC server.
pub(crate) fn named() -> Vec<Named> {
    vec![Named {
        name: "hid",
        root: Arc::new(HidServer),
    }]
}

/// The shared memory of the service. It is created on the first call and filled at once.
fn shared_memory() -> Result<SharedMemory, u32> {
    with(|input| -> Result<SharedMemory, u32> {
        if let Some(shm) = input.shm {
            return Ok(shm);
        }
        let shm = SharedMemory::create()?;
        input.shm = Some(shm);
        let _ = shmem::fill(input);
        Ok(shm)
    })
}

impl Service for AppletResource {
    fn name(&self) -> &'static str {
        "IAppletResource"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => {
                let mut cached = self.handle.lock().unwrap_or_else(|e| e.into_inner());
                if cached.is_none() {
                    match shared_memory() {
                        Ok(shm) => *cached = Some(shm.handle),
                        Err(code) => {
                            rsp.result = code;
                            return Handled::Done;
                        }
                    }
                }
                rsp.copy_handles.extend(*cached);
                Handled::Done
            }
            _ => Handled::Unknown,
        }
    }
}

fn applet_resource() -> Arc<dyn Service> {
    static RESOURCE: OnceLock<Arc<AppletResource>> = OnceLock::new();
    RESOURCE
        .get_or_init(|| {
            Arc::new(AppletResource {
                handle: Mutex::new(None),
            })
        })
        .clone()
}

/// The ids of a `SetSupportedNpadIdType` request.
fn read_ids(req: &Request<'_>) -> Option<Vec<u32>> {
    let buf = req
        .buffer(BufferKind::Send, 0)
        .or_else(|| req.buffer(BufferKind::Pointer, 0))?;
    let n = (buf.size as usize / 4).min(MAX_IDS);
    let mut bytes = vec![0u8; n * 4];
    guest::read(buf.addr, &mut bytes).ok()?;
    Some(
        bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

fn value_at(req: &Request<'_>, offset: usize) -> Option<VibrationValue> {
    let b = req.data.get(offset..offset + 16)?;
    let mut a = [0u8; 16];
    a.copy_from_slice(b);
    Some(VibrationValue::from_bytes(&a))
}

impl Service for HidServer {
    fn name(&self) -> &'static str {
        "IHidServer"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => rsp.objects.push(applet_resource()),
            // Activate: DebugPad, TouchScreen, Mouse, Keyboard, Npad. Deactivate: Npad.
            1 | 11 | 21 | 31 | 103 | 104 | 109 | 300 => {}
            100 => {
                let Some(styles) = req.u32_at(0) else {
                    rsp.result = RESULT_INVALID;
                    return Handled::Done;
                };
                with(|i| i.style_set = styles);
            }
            101 => rsp.push_u32(with(|i| i.style_set)),
            102 => {
                let Some(ids) = read_ids(req) else {
                    rsp.result = RESULT_INVALID;
                    return Handled::Done;
                };
                with(|i| i.set_ids(&ids));
            }
            106 => {
                let Some(id) = req.u32_at(0) else {
                    rsp.result = RESULT_INVALID;
                    return Handled::Done;
                };
                match style_event(id) {
                    Ok(readable) => rsp.copy_handles.push(readable),
                    Err(code) => rsp.result = code,
                }
            }
            108 => {
                let id = req.u32_at(0).unwrap_or(0);
                rsp.push_u64(u64::from(crate::exports::player_led_pattern(id)));
            }
            120 => {
                let kind = req.u64_at(8).unwrap_or(0) as u32;
                with(|i| i.hold_type = kind);
            }
            121 => rsp.push_u64(u64::from(with(|i| i.hold_type))),
            128 => {
                let mode = req.u64_at(8).unwrap_or(0) as u32;
                with(|i| i.handheld_activation = mode);
            }
            129 => rsp.push_u64(u64::from(with(|i| i.handheld_activation))),
            200 => {
                let handle = req.u32_at(0).unwrap_or(0);
                let (kind, location) = crate::exports::vibration_info(handle);
                rsp.push_u32(kind);
                rsp.push_u32(location);
            }
            201 => {
                let handle = req.u32_at(0).unwrap_or(0);
                if let Some(value) = value_at(req, 4) {
                    with(|i| i.send_vibration(handle, value));
                }
            }
            202 => {
                let handle = req.u32_at(0).unwrap_or(0);
                rsp.push_bytes(&with(|i| i.actual_vibration(handle)).to_bytes());
            }
            204 => {
                let on = req.u32_at(0).unwrap_or(0) & 1 == 1;
                with(|i| i.vibration_permitted = on);
            }
            205 => rsp.push_u32(u32::from(with(|i| {
                i.vibration_permitted && i.rumble_available()
            }))),
            // StartSixAxisSensor, StopSixAxisSensor, EnableSixAxisSensorFusion, SetSixAxisSensorFusionParameters,
            // ResetSixAxisSensorFusionParameters, SetGyroscopeZeroDriftMode, ResetGyroscopeZeroDriftMode.
            301 | 302 | 304 | 305 | 307 | 308 | 310 => {}
            // IsSixAxisSensorFusionEnabled.
            303 => rsp.push_u32(0),
            // GetSixAxisSensorFusionParameters.
            306 => {
                rsp.push_u32(0);
                rsp.push_u32(0);
            }
            // GetGyroscopeZeroDriftMode: standard.
            309 => rsp.push_u32(1),
            311 => {
                let handle = req.u32_at(0).unwrap_or(0);
                rsp.push_u32(u32::from(with(|i| i.handle_at_rest(handle))));
            }
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// The readable handle of the style update event. The service signals the writable one when a controller comes or
/// goes. The event is created once for all ids.
fn style_event(id: u32) -> Result<u32, u32> {
    if id_bit(id) == 0 {
        return Err(RESULT_INVALID);
    }
    with(|i| -> Result<u32, u32> {
        if let Some((_, readable)) = i.style_event {
            return Ok(readable);
        }
        let kernel = kernel::kernel().ok_or(shmem::NO_KERNEL)?;
        let (writable, readable) = kernel.create_event()?;
        i.style_event = Some((writable, readable));
        Ok(readable)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::guest_memory;
    use nxkit_abi::ipc::Buffer;

    fn call(command: u32, data: &[u8], buffers: &[Buffer]) -> (Handled, Response) {
        let req = Request {
            command,
            data,
            pid: None,
            copy_handles: &[],
            move_handles: &[],
            buffers,
            objects: &[],
        };
        let mut rsp = Response::default();
        let handled = HidServer.handle(&req, &mut rsp);
        (handled, rsp)
    }

    #[test]
    fn the_service_is_named_hid() {
        let n = named();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].name, "hid");
    }

    #[test]
    fn create_applet_resource_returns_an_object() {
        let (h, rsp) = call(0, &[0; 8], &[]);
        assert_eq!(h, Handled::Done);
        assert_eq!(rsp.objects.len(), 1);
        assert_eq!(rsp.objects[0].name(), "IAppletResource");
    }

    #[test]
    fn unknown_commands_stop_the_game() {
        assert_eq!(call(9999, &[], &[]).0, Handled::Unknown);
    }

    #[test]
    fn the_style_set_round_trips() {
        let mut data = [0u8; 16];
        data[0..4].copy_from_slice(&0x13u32.to_le_bytes());
        assert_eq!(call(100, &data, &[]).0, Handled::Done);
        let (_, rsp) = call(101, &[0; 8], &[]);
        assert_eq!(rsp.data, 0x13u32.to_le_bytes());
    }

    #[test]
    fn supported_ids_are_read_from_the_buffer() {
        let (base, _guard) = guest_memory();
        for (i, id) in [0u32, 0x20].into_iter().enumerate() {
            guest::write_u32(base + 4 * i as u64, id).ok();
        }
        let buf = Buffer {
            addr: base,
            size: 8,
            kind: BufferKind::Send,
            index: 0,
        };
        let (h, rsp) = call(102, &[0; 8], &[buf]);
        assert_eq!((h, rsp.result), (Handled::Done, 0));
        assert_eq!(with(|i| i.ids), 1 | 1 << 8);
        with(|i| i.set_ids(&[0, 1, 2, 3, 4, 5, 6, 7, 0x20, 0x10]));
    }

    #[test]
    fn the_hold_type_round_trips() {
        let mut data = [0u8; 16];
        data[8..16].copy_from_slice(&1u64.to_le_bytes());
        call(120, &data, &[]);
        let (_, rsp) = call(121, &[0; 8], &[]);
        assert_eq!(rsp.data, 1u64.to_le_bytes());
    }

    #[test]
    fn the_led_pattern_answers() {
        let (_, rsp) = call(108, &1u32.to_le_bytes(), &[]);
        assert_eq!(rsp.data, 3u64.to_le_bytes());
    }
}
