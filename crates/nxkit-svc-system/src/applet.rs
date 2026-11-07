//! The `appletOE` and `appletAE` services with `IApplicationProxy`, `ICommonStateGetter` and `ISelfController`.
//!
//! IDs and layouts are from libnx `applet.c` and switchbrew `Applet Manager services`. The values follow nxnative
//! `dm/host/hle_oe.c` (the messages, the focus handling modes, the display size, the performance configuration).
//!
//! `appletOE`: 0 `OpenApplicationProxy`. `appletAE`: 350 `OpenSystemApplicationProxy` and 1000 `GetDebugFunctions`.
//!
//! `IApplicationProxy`: 0 `GetCommonStateGetter`, 1 `GetSelfController`, 2 `GetWindowController`, 3
//! `GetAudioController`, 4 `GetDisplayController`, 11 `GetLibraryAppletCreator`, 20 `GetApplicationFunctions` and 1000
//! `GetDebugFunctions`. The objects are in `applet_ifaces`.
//!
//! `ICommonStateGetter`: 0 `GetEventHandle`, 1 `ReceiveMessage`, 3 and 4 `AllowToEnterSleep` and `DisallowToEnterSleep`,
//! 5 `GetOperationMode`, 6 `GetPerformanceMode`, 8 `GetBootMode`, 9 `GetCurrentFocusState`, 10 to 12 the sleep locks, 50
//! `IsVrModeEnabled`, 51 `SetVrModeEnabled`, 52 `SetLcdBacklightOffEnabled`, 60 `GetDefaultDisplayResolution`, 61
//! `GetDefaultDisplayResolutionChangeEvent`, 66 `SetCpuBoostMode`, 90 `SetPerformanceConfigurationChangedNotification`
//! and 91 `GetCurrentPerformanceConfiguration`.
//!
//! `ISelfController`: 0 to 4 `Exit`, `LockExit`, `UnlockExit`, `EnterFatalSection` and `LeaveFatalSection`, 9
//! `GetLibraryAppletLaunchableEvent`, 10 `SetScreenShotPermission`, 11 `SetOperationModeChangedNotification`, 12
//! `SetPerformanceModeChangedNotification`, 13 `SetFocusHandlingMode`, 14 `SetRestartMessageEnabled`, 16 to 20 the
//! out-of-focus, firmware update, capture button, album orientation and keyboard layout settings, 50 and 51
//! `SetHandlesRequestToDisplay` and `ApproveToDisplay`, 60 `OverrideAutoSleepTimeAndDimmingTime`, 61 to 65 the media,
//! idle detection and input detection settings, 68 and 69 `SetAutoSleepDisabled` and `IsAutoSleepDisabled`, 70 `ReportMultimediaError`, 72
//! `SetInputDetectionPolicy`, 80 `SetWirelessPriorityMode`, 90 and 91 the accumulated suspended tick, 100
//! `SetAlbumImageTakenNotificationEnabled`, 110 `SetApplicationAlbumUserData`, 120 `SaveCurrentScreenshot` and 130
//! `SetRecordVolumeMuted`. The display layer commands 40 to 46 belong to the video side and are unknown here.
//! `Exit` is logged. The host decides when the process ends.

use crate::applet_ifaces::{
    ApplicationFunctions, AudioController, DebugFunctions, DisplayController, LibraryAppletCreator,
    WindowController,
};
use crate::state::state;
use crate::util::{Event, RES_FAIL, RES_NO_MESSAGE, fail};
use nxkit_abi::ipc::{Handled, Request, Response, Service};
use std::sync::Arc;

/// `appletOE` and `appletAE`.
pub(crate) struct Applet {
    pub(crate) name: &'static str,
}

impl Service for Applet {
    fn name(&self) -> &'static str {
        self.name
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        let object: Arc<dyn Service> = match (self.name, req.command) {
            ("appletOE", 0) | ("appletAE", 350) => Arc::new(ApplicationProxy),
            ("appletAE", 1000) => Arc::new(DebugFunctions),
            _ => return Handled::Unknown,
        };
        rsp.objects.push(object);
        Handled::Done
    }
}

/// `nn::am::service::IApplicationProxy`.
struct ApplicationProxy;

impl Service for ApplicationProxy {
    fn name(&self) -> &'static str {
        "applet::IApplicationProxy"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        let object: Arc<dyn Service> = match req.command {
            0 => Arc::new(CommonStateGetter {
                resolution_event: Event::new(),
            }),
            1 => Arc::new(SelfController {
                launchable: Event::new(),
                suspended_tick: Event::new(),
            }),
            2 => Arc::new(WindowController),
            3 => Arc::new(AudioController),
            4 => Arc::new(DisplayController),
            11 => Arc::new(LibraryAppletCreator),
            20 => Arc::new(ApplicationFunctions {
                gpu_error: Event::new(),
                health_warning: Event::new(),
            }),
            1000 => Arc::new(DebugFunctions),
            _ => return Handled::Unknown,
        };
        rsp.objects.push(object);
        Handled::Done
    }
}

/// A boolean argument of a command.
fn bool_at(req: &Request<'_>, offset: usize) -> bool {
    req.data.get(offset).is_some_and(|b| *b != 0)
}

/// `nn::am::service::ICommonStateGetter`.
struct CommonStateGetter {
    resolution_event: Event,
}

impl Service for CommonStateGetter {
    fn name(&self) -> &'static str {
        "applet::ICommonStateGetter"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        let s = state();
        match req.command {
            0 => {
                return match s.event_handle() {
                    Some(h) => {
                        rsp.copy_handles.push(h);
                        Handled::Done
                    }
                    None => fail(rsp, RES_FAIL),
                };
            }
            1 => match s.pop() {
                Some(m) => rsp.push_u32(m),
                None => return fail(rsp, RES_NO_MESSAGE),
            },
            3 | 4 | 10 | 11 | 12 | 51 | 52 | 66 | 90 => {}
            5 => rsp.push_bytes(&[s.operation_mode() as u8]),
            6 => rsp.push_u32(s.performance_mode()),
            8 | 50 => rsp.push_bytes(&[0]),
            9 => rsp.push_bytes(&[s.with(|i| i.focus) as u8]),
            60 => {
                let (w, h) = if s.with(|i| i.handheld) {
                    (1280u32, 720u32)
                } else {
                    (1920, 1080)
                };
                rsp.push_u32(w);
                rsp.push_u32(h);
            }
            61 => return self.resolution_event.reply(rsp, false),
            91 => rsp.push_u32(if s.performance_mode() == 1 {
                0x0001_0001
            } else {
                0x0001_0000
            }),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}

/// `nn::am::service::ISelfController`.
struct SelfController {
    launchable: Event,
    suspended_tick: Event,
}

impl Service for SelfController {
    fn name(&self) -> &'static str {
        "applet::ISelfController"
    }

    fn handle(&self, req: &Request<'_>, rsp: &mut Response) -> Handled {
        let s = state();
        match req.command {
            0 => eprintln!("nxkit-svc-system: ISelfController::Exit"),
            1..=4 | 10 | 14 | 16..=20 | 50 | 51 | 60 | 61 | 64 | 65 | 70 | 72 | 80 | 100 | 110 | 120
            | 130 => {}
            9 => return self.launchable.reply(rsp, true),
            11 => s.with(|i| i.notify_operation = bool_at(req, 0)),
            12 => s.with(|i| i.notify_performance = bool_at(req, 0)),
            // The first flag is the focus notification. nxnative maps the modes 1 and 2 to it.
            13 => s.with(|i| i.notify_focus = bool_at(req, 0)),
            62 => s.with(|i| i.idle_extension = req.u32_at(0).unwrap_or(0)),
            63 => rsp.push_u32(s.with(|i| i.idle_extension)),
            68 => s.with(|i| i.auto_sleep_disabled = bool_at(req, 0)),
            69 => rsp.push_bytes(&[u8::from(s.with(|i| i.auto_sleep_disabled))]),
            90 => rsp.push_u64(0),
            91 => return self.suspended_tick.reply(rsp, false),
            _ => return Handled::Unknown,
        }
        Handled::Done
    }
}
