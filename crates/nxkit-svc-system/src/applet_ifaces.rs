//! The interfaces of `IApplicationProxy` that need no shared state: `IWindowController`, `IAudioController`,
//! `IDisplayController`, `IDebugFunctions`, `ILibraryAppletCreator` and `IApplicationFunctions` with its storage.
//!
//! IDs and layouts are from libnx `applet.c` and switchbrew `Applet Manager services`. The values follow the engine
//! `dm/host/hle_oe.c` (`hle_sdk16.c` for the display version).
//!
//! `IWindowController`: 1 `GetAppletResourceUserId` gives `0x80`, 10 to 12 and 20 to 21 are accepted.
//!
//! `IAudioController`: 0 `SetExpectedMasterVolume`, 3 `ChangeMainAppletMasterVolume` and 4 `SetTransparentVolumeRate`
//! are accepted, 1 and 2 give a volume of 1.0.
//!
//! `IDisplayController`: 8 `TakeScreenShotOfOwnLayer`, 20 `ClearCaptureBuffer`, 21 `ClearAppletTransitionBuffer` and 28
//! `TakeScreenShotOfOwnLayerEx` are accepted, as the screenshot calls of the engine.
//!
//! `IDebugFunctions`: 0, 10, 20 and 100 are accepted.
//!
//! `ILibraryAppletCreator`: 1 `TerminateAllLibraryApplets` is accepted and 2 `AreAnyLibraryAppletsLeft` gives false.
//! The host runs no library applets, so creating an applet or a storage is unknown.
//!
//! `IApplicationFunctions`: 1 `PopLaunchParameter`, 20 `EnsureSaveData`, 21 `GetDesiredLanguage`, 22
//! `SetTerminateResult`, 23 `GetDisplayVersion`, 30 to 33 the HOME button blocks, 40 `NotifyRunning`, 50
//! `GetPseudoDeviceId`, 60 `SetMediaPlaybackStateForApplication`, 65 to 67 the game play recording, 90
//! `EnableApplicationCrashReport`, 100 to 102 the copyright frame buffer, 123 `GetPreviousProgramIndex`, 124
//! `EnableApplicationAllThreadDumpOnCrash`, 130 `GetGpuErrorDetectedSystemEvent`, 160
//! `GetHealthWarningDisappearedSystemEvent`, 330 `IsLanguageSelectionLimited` and 1001 `PrepareForJit`.
//!
//! The storage that `PopLaunchParameter` returns: 0 `Open` gives the accessor with 0 `GetSize`, 10 `Write` and 11 `Read`.

use crate::acc::UID;
use crate::config;
use crate::set::language_code;
use crate::util::{Event, RES_FAIL, fail, padded, result, write_out};
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use std::sync::Arc;

/// The ID of the applet resource user of the game, `0x80` in the engine.
const ARUID: u64 = 0x80;

/// The 16 bytes of `GetPseudoDeviceId`.
const PSEUDO_DEVICE_ID: [u8; 16] = [
    0x78, 0x62, 0x6c, 0x00, 0x53, 0x57, 0x4e, 0x41, 0x54, 0x49, 0x56, 0x45, 0x31, 0x30, 0x30, 0x31,
];

/// `PopLaunchParameter` of the kind `UserChannel` when the host has no parameter: `MAKERESULT(128, 1)`.
const RES_NO_PARAMETER: u32 = result(128, 1);

/// The magic of the data of the preselected user.
const PRESELECTED_MAGIC: u32 = 0xc794_97ca;

/// `nn::am::service::IWindowController`.
pub(crate) struct WindowController;

impl Service for WindowController {
    fn name(&self) -> &'static str {
        "applet::IWindowController"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            1 => rsp.push_u64(ARUID),
            10..=12 | 20 | 21 => {}
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// `nn::am::service::IAudioController`.
pub(crate) struct AudioController;

impl Service for AudioController {
    fn name(&self) -> &'static str {
        "applet::IAudioController"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 | 3 | 4 => {}
            1 | 2 => rsp.push_u32(1.0f32.to_bits()),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// `nn::am::service::IDisplayController`.
pub(crate) struct DisplayController;

impl Service for DisplayController {
    fn name(&self) -> &'static str {
        "applet::IDisplayController"
    }

    fn handle(&self, req: &Request<'_>, _rsp: &mut Response) -> Handled {
        match req.command {
            8 | 20 | 21 | 28 => Handled::Done,
            _ => Handled::Unknown,
        }
    }
}

/// `nn::am::service::IDebugFunctions`.
pub(crate) struct DebugFunctions;

impl Service for DebugFunctions {
    fn name(&self) -> &'static str {
        "applet::IDebugFunctions"
    }

    fn handle(&self, req: &Request<'_>, _rsp: &mut Response) -> Handled {
        match req.command {
            0 | 10 | 20 | 100 => Handled::Done,
            _ => Handled::Unknown,
        }
    }
}

/// `nn::am::service::ILibraryAppletCreator`.
pub(crate) struct LibraryAppletCreator;

impl Service for LibraryAppletCreator {
    fn name(&self) -> &'static str {
        "applet::ILibraryAppletCreator"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            1 => {}
            2 => rsp.push_bytes(&[0]),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// The 0x88 bytes of the launch parameter of the preselected user.
fn preselected_user() -> Vec<u8> {
    let mut b = vec![0u8; 0x88];
    b[..4].copy_from_slice(&PRESELECTED_MAGIC.to_le_bytes());
    b[4..8].copy_from_slice(&1u32.to_le_bytes());
    b[8..16].copy_from_slice(&UID[0].to_le_bytes());
    b[16..24].copy_from_slice(&UID[1].to_le_bytes());
    b
}

/// `nn::am::service::IApplicationFunctions`.
pub(crate) struct ApplicationFunctions {
    pub(crate) gpu_error: Event,
    pub(crate) health_warning: Event,
}

impl ApplicationFunctions {
    fn pop_launch_parameter(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        let data = match req.u32_at(0) {
            Some(1) => match &config().launch_parameter {
                Some(p) => p.clone(),
                None => return fail(rsp, RES_NO_PARAMETER),
            },
            Some(2) => preselected_user(),
            _ => return fail(rsp, RES_NO_PARAMETER),
        };
        rsp.objects.push(Arc::new(Storage { data }));
        Handled::Done
    }
}

impl Service for ApplicationFunctions {
    fn name(&self) -> &'static str {
        "applet::IApplicationFunctions"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            1 => return self.pop_launch_parameter(req, rsp),
            20 => rsp.push_u64(0),
            21 => rsp.push_u64(language_code(&config().language)),
            22 => eprintln!(
                "nxkit-svc-system: SetTerminateResult {:#x}",
                req.u32_at(0).unwrap_or(0)
            ),
            23 => rsp.push_bytes(&padded::<16>(&config().display_version)),
            30..=33 | 60 | 66 | 67 | 90 | 100..=102 | 124 | 1001 => {}
            40 => rsp.push_bytes(&[1]),
            50 => rsp.push_bytes(&PSEUDO_DEVICE_ID),
            65 | 330 => rsp.push_bytes(&[0]),
            123 => rsp.push_u32(u32::MAX),
            130 => return self.gpu_error.reply(rsp, false),
            160 => return self.health_warning.reply(rsp, false),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// `nn::am::service::IStorage` with a fixed content.
struct Storage {
    data: Vec<u8>,
}

impl Service for Storage {
    fn name(&self) -> &'static str {
        "applet::IStorage"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => {
                rsp.objects.push(Arc::new(StorageAccessor {
                    data: self.data.clone(),
                }));
                Handled::Done
            }
            _ => Handled::Unknown,
        }
    }
}

/// `nn::am::service::IStorageAccessor`.
struct StorageAccessor {
    data: Vec<u8>,
}

impl Service for StorageAccessor {
    fn name(&self) -> &'static str {
        "applet::IStorageAccessor"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        match req.command {
            0 => rsp.push_u64(self.data.len() as u64),
            10 => {}
            11 => {
                let Some(offset) = req.u64_at(0).and_then(|o| usize::try_from(o).ok()) else {
                    return fail(rsp, RES_FAIL);
                };
                let Some(part) = self.data.get(offset..) else {
                    return fail(rsp, RES_FAIL);
                };
                let _ = write_out(req, 0, part);
            }
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preselected_user_data_names_the_local_user() {
        let b = preselected_user();
        assert_eq!(b.len(), 0x88);
        assert_eq!(&b[..4], &0xc794_97cau32.to_le_bytes());
        assert_eq!(&b[8..16], &UID[0].to_le_bytes());
    }

    #[test]
    fn a_missing_parameter_is_the_applet_result() {
        assert_eq!(RES_NO_PARAMETER, 0x280);
    }
}
