//! The output unit: AUHAL on macOS, RemoteIO on iOS.
//!
//! The unit calls one closure on the device's real-time thread. The first call joins that thread to the `os_workgroup` of
//! the device, so the scheduler treats it as part of the device's deadline (R20).

#![allow(unsafe_code)]

/// The closure the device thread runs. It fills an interleaved `f32` buffer, with the width that [`Output::start`]
/// was given, with silence or sound.
pub type Fill = Box<dyn FnMut(&mut [f32]) + Send>;

#[cfg(target_vendor = "apple")]
mod imp {
    use super::Fill;
    use crate::sys::spatial::{self, Spatial};
    use objc2_audio_toolbox::{
        AudioComponentDescription, AudioComponentFindNext, AudioComponentInstance,
        AudioComponentInstanceNew, AudioOutputUnitStart, AudioUnitGetProperty, AudioUnitInitialize,
        AudioUnitRender, AudioUnitSetProperty,
    };
    #[cfg(target_os = "macos")]
    use objc2_core_audio::{AudioObjectGetPropertyData, AudioObjectPropertyAddress};
    use objc2_core_audio_types::{AudioBufferList, AudioStreamBasicDescription};
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};

    const SCOPE_GLOBAL: u32 = 0;
    const SCOPE_INPUT: u32 = 1;
    const PROP_STREAM_FORMAT: u32 = 8;
    const PROP_SET_RENDER_CALLBACK: u32 = 23;
    #[cfg(target_os = "macos")]
    const PROP_CURRENT_DEVICE: u32 = 2000;
    const PROP_OS_WORKGROUP: u32 = 2015;
    #[cfg(target_os = "macos")]
    const SYSTEM_OBJECT: u32 = 1;
    #[cfg(target_os = "macos")]
    const SELECTOR_DEFAULT_OUTPUT: u32 = 0x644f_7574;
    #[cfg(target_os = "macos")]
    const SCOPE_HAL_GLOBAL: u32 = 0x676c_6f62;

    unsafe extern "C" {
        fn os_workgroup_join(wg: *mut c_void, token: *mut u64) -> i32;
        #[cfg(target_os = "macos")]
        fn AudioObjectAddPropertyListener(
            object: u32,
            address: *const AudioObjectPropertyAddress,
            listener: unsafe extern "C" fn(
                u32,
                u32,
                *const AudioObjectPropertyAddress,
                *mut c_void,
            ) -> i32,
            data: *mut c_void,
        ) -> i32;
        #[cfg(target_os = "macos")]
        fn AudioObjectSetPropertyData(
            object: u32,
            address: *const AudioObjectPropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: u32,
            data: *const c_void,
        ) -> i32;
    }

    /// `kAudioDevicePropertyBufferFrameSize`.
    #[cfg(target_os = "macos")]
    const SELECTOR_BUFFER_FRAMES: u32 = 0x6673_697a;
    /// `kAudioDevicePropertyTransportType`.
    #[cfg(target_os = "macos")]
    const SELECTOR_TRANSPORT: u32 = 0x7472_616e;
    /// `kAudioDeviceProcessorOverload`.
    #[cfg(target_os = "macos")]
    const SELECTOR_OVERLOAD: u32 = 0x6f76_6572;

    #[cfg(target_os = "macos")]
    static OVERLOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// Counts the times the device's IO thread missed its deadline. The listener runs on a notification thread of
    /// Core Audio, not on the IO thread. Each overload is an audible gap, whatever the samples were.
    #[cfg(target_os = "macos")]
    unsafe extern "C" fn overload(
        _object: u32,
        _n: u32,
        _addresses: *const AudioObjectPropertyAddress,
        _data: *mut c_void,
    ) -> i32 {
        let n = OVERLOADS.fetch_add(1, Ordering::Relaxed) + 1;
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() % 1_000_000);
        eprintln!("nxkit-audio: the output device missed its deadline (overload {n}) at t {ms}");
        0
    }

    #[cfg(target_os = "macos")]
    fn device_u32(device: u32, selector: u32) -> Option<u32> {
        let addr = AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: SCOPE_HAL_GLOBAL,
            mElement: 0,
        };
        let (mut v, mut size) = (0u32, 4u32);
        // SAFETY: `v` has the 4 bytes the call writes and `size` says so.
        let st = unsafe {
            AudioObjectGetPropertyData(
                device,
                &addr,
                0,
                std::ptr::null(),
                &mut size,
                NonNull::from(&mut v).cast::<c_void>(),
            )
        };
        (st == 0).then_some(v)
    }

    /// Logs the transport and the IO buffer of the device, sets the buffer from `NXKIT_AUDIO_DEVICE_FRAMES`, and listens
    /// for overloads of the device.
    #[cfg(target_os = "macos")]
    fn watch_device(device: u32) {
        if let Some(frames) = crate::settings::settings().device_frames {
            let addr = AudioObjectPropertyAddress {
                mSelector: SELECTOR_BUFFER_FRAMES,
                mScope: SCOPE_HAL_GLOBAL,
                mElement: 0,
            };
            // SAFETY: `frames` is the 4 bytes that the property takes.
            let st = unsafe {
                AudioObjectSetPropertyData(
                    device,
                    &addr,
                    0,
                    std::ptr::null(),
                    4,
                    std::ptr::from_ref(&frames).cast::<c_void>(),
                )
            };
            if st != 0 {
                eprintln!(
                    "nxkit-audio: the device refused an IO buffer of {frames} frames (status {st})"
                );
            }
        }
        let transport = device_u32(device, SELECTOR_TRANSPORT).map_or_else(
            || "?".to_string(),
            |t| {
                t.to_be_bytes()
                    .iter()
                    .map(|b| char::from(*b))
                    .collect::<String>()
            },
        );
        let frames = device_u32(device, SELECTOR_BUFFER_FRAMES).unwrap_or(0);
        eprintln!(
            "nxkit-audio: the output device has transport '{transport}' and an IO buffer of {frames} frames ({:.1} ms)",
            f64::from(frames) / 48.0
        );
        let addr = AudioObjectPropertyAddress {
            mSelector: SELECTOR_OVERLOAD,
            mScope: SCOPE_HAL_GLOBAL,
            mElement: 0,
        };
        // SAFETY: the listener is a plain function that lives for the process, and it uses no data.
        let st = unsafe {
            AudioObjectAddPropertyListener(device, &addr, overload, std::ptr::null_mut())
        };
        if st != 0 {
            eprintln!("nxkit-audio: no overload listener on the output device (status {st})");
        }
    }

    /// The callback context. It is boxed and never freed, because the unit runs until the process ends.
    struct Ctx {
        fill: Fill,
        /// The spatial mixer that the callback renders, or null when `fill` plays straight to the device.
        spatial: AudioComponentInstance,
        workgroup: AtomicPtr<c_void>,
        join_status: AtomicI32,
        joined: bool,
        token: [u64; 8],
    }

    /// The running output unit.
    pub struct Output {
        ctx: *const Ctx,
        mode: &'static str,
    }

    // SAFETY: the context is only read through atomics from other threads, and the audio thread owns the rest.
    unsafe impl Send for Output {}
    // SAFETY: see above.
    unsafe impl Sync for Output {}

    unsafe extern "C-unwind" fn render(
        refcon: NonNull<c_void>,
        _flags: NonNull<objc2_audio_toolbox::AudioUnitRenderActionFlags>,
        _stamp: NonNull<objc2_core_audio_types::AudioTimeStamp>,
        _bus: u32,
        _frames: u32,
        data: *mut AudioBufferList,
    ) -> i32 {
        if data.is_null() {
            return 0;
        }
        // SAFETY: `refcon` is the leaked `Ctx` that `start` made. Only this thread touches the non-atomic fields.
        let ctx = unsafe { &mut *refcon.as_ptr().cast::<Ctx>() };
        join(ctx);
        // SAFETY: the unit hands us a valid list that lives during this call.
        let list = unsafe { &*data };
        if list.mNumberBuffers == 0 {
            return 0;
        }
        let buf = &list.mBuffers[0];
        let len = (buf.mDataByteSize / 4) as usize;
        if buf.mData.is_null() || len == 0 || !len.is_multiple_of(2) {
            return 0;
        }
        // SAFETY: `mData` points at `mDataByteSize` bytes of `f32` that this call owns until it returns.
        let out = unsafe { std::slice::from_raw_parts_mut(buf.mData.cast::<f32>(), len) };
        (ctx.fill)(out);
        0
    }

    /// The callback with spatial audio: the spatial mixer renders into the device's two buffers.
    unsafe extern "C-unwind" fn render_spatial(
        refcon: NonNull<c_void>,
        flags: NonNull<objc2_audio_toolbox::AudioUnitRenderActionFlags>,
        stamp: NonNull<objc2_core_audio_types::AudioTimeStamp>,
        _bus: u32,
        frames: u32,
        data: *mut AudioBufferList,
    ) -> i32 {
        let Some(list) = NonNull::new(data) else {
            return 0;
        };
        // SAFETY: as in `render`.
        let ctx = unsafe { &mut *refcon.as_ptr().cast::<Ctx>() };
        join(ctx);
        // SAFETY: the mixer is initialized and renders into the list that the device gave this call. `flags` and
        // `stamp` live during this call.
        unsafe {
            AudioUnitRender(
                ctx.spatial,
                Some(&mut *flags.as_ptr()),
                stamp.as_ref(),
                0,
                frames,
                list,
            )
        }
    }

    /// Joins the device thread to the workgroup of the device, once.
    fn join(ctx: &mut Ctx) {
        if !ctx.joined {
            ctx.joined = true;
            let wg = ctx.workgroup.load(Ordering::Acquire);
            let status = if wg.is_null() {
                -1
            } else {
                // SAFETY: `wg` is the live workgroup of the device and `token` has the room the call needs.
                unsafe { os_workgroup_join(wg, ctx.token.as_mut_ptr()) }
            };
            ctx.join_status.store(status, Ordering::Release);
        }
    }

    fn check(what: &str, status: i32) -> Result<(), String> {
        if status == 0 {
            Ok(())
        } else {
            Err(format!("{what} failed with status {status}"))
        }
    }

    #[cfg(target_os = "macos")]
    fn default_device() -> Result<u32, String> {
        let addr = AudioObjectPropertyAddress {
            mSelector: SELECTOR_DEFAULT_OUTPUT,
            mScope: SCOPE_HAL_GLOBAL,
            mElement: 0,
        };
        let mut dev: u32 = 0;
        let mut size: u32 = 4;
        // SAFETY: `dev` has the 4 bytes the call writes and `size` says so.
        let st = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &addr,
                0,
                std::ptr::null(),
                &mut size,
                NonNull::from(&mut dev).cast::<c_void>(),
            )
        };
        check("get the default output device", st)?;
        if dev == 0 {
            return Err("there is no default output device".to_string());
        }
        Ok(dev)
    }

    impl Output {
        /// Opens the default output device at 48 kHz `f32`, joins its workgroup and starts it. `fill` writes frames of
        /// `width` channels: 2, or 6 (5.1), which play as spatial audio where the device has it and are mixed down
        /// to stereo elsewhere.
        pub fn start(fill: Fill, width: usize) -> Result<Output, String> {
            #[cfg(target_os = "macos")]
            let device = default_device()?;
            #[cfg(target_os = "macos")]
            watch_device(device);
            #[cfg(target_os = "macos")]
            let (fill, sp, mode) = if width == 6 {
                route(device, fill)
            } else {
                (fill, None, "stereo")
            };
            #[cfg(target_os = "ios")]
            let (fill, sp, mode): (Fill, Option<Spatial>, _) = if width == 6 {
                (stereo_downmix(fill), None, "5.1 mixed down to stereo")
            } else {
                (fill, None, "stereo")
            };
            let desc = AudioComponentDescription {
                componentType: 0x6175_6f75,
                componentSubType: if cfg!(target_os = "ios") {
                    0x7269_6f63
                } else {
                    0x6168_616c
                },
                componentManufacturer: 0x6170_706c,
                componentFlags: 0,
                componentFlagsMask: 0,
            };
            // SAFETY: a null start component and a valid description.
            let comp = unsafe { AudioComponentFindNext(std::ptr::null_mut(), &desc) };
            if comp.is_null() {
                return Err("no audio output component".to_string());
            }
            let mut unit: AudioComponentInstance = std::ptr::null_mut();
            // SAFETY: `comp` is a live component and `unit` receives the instance.
            check("create the output unit", unsafe {
                AudioComponentInstanceNew(comp, &mut unit)
            })?;

            // SAFETY: `device` is 4 bytes, as `CurrentDevice` needs.
            #[cfg(target_os = "macos")]
            check("set the output device", unsafe {
                AudioUnitSetProperty(
                    unit,
                    PROP_CURRENT_DEVICE,
                    SCOPE_GLOBAL,
                    0,
                    std::ptr::from_ref(&device).cast::<c_void>(),
                    4,
                )
            })?;
            // The spatial mixer renders one buffer per channel.
            let fmt = spatial::format(2, sp.is_none());
            let fmt_size =
                u32::try_from(std::mem::size_of::<AudioStreamBasicDescription>()).unwrap_or(40);
            // SAFETY: `fmt` is a complete stream description of `fmt_size` bytes.
            check("set the stream format", unsafe {
                AudioUnitSetProperty(
                    unit,
                    PROP_STREAM_FORMAT,
                    SCOPE_INPUT,
                    0,
                    std::ptr::from_ref(&fmt).cast::<c_void>(),
                    fmt_size,
                )
            })?;

            let ctx: &'static mut Ctx = Box::leak(Box::new(Ctx {
                fill,
                spatial: sp.as_ref().map_or(std::ptr::null_mut(), |s| s.unit),
                workgroup: AtomicPtr::new(std::ptr::null_mut()),
                join_status: AtomicI32::new(i32::MIN),
                joined: false,
                token: [0; 8],
            }));
            let proc_ = if sp.is_some() {
                render_spatial as *const c_void
            } else {
                render as *const c_void
            };
            let cb = CallbackStruct {
                proc_,
                refcon: std::ptr::from_mut(ctx).cast::<c_void>(),
            };
            // SAFETY: `cb` has the layout of `AURenderCallbackStruct` and `render` has the callback signature.
            check("set the render callback", unsafe {
                AudioUnitSetProperty(
                    unit,
                    PROP_SET_RENDER_CALLBACK,
                    SCOPE_GLOBAL,
                    0,
                    std::ptr::from_ref(&cb).cast::<c_void>(),
                    16,
                )
            })?;
            // SAFETY: the unit is fully configured.
            check("initialize the output unit", unsafe {
                AudioUnitInitialize(unit)
            })?;

            let mut wg: *mut c_void = std::ptr::null_mut();
            let mut size: u32 = 8;
            // SAFETY: `wg` has the 8 bytes of a pointer and `size` says so.
            let st = unsafe {
                AudioUnitGetProperty(
                    unit,
                    PROP_OS_WORKGROUP,
                    SCOPE_GLOBAL,
                    0,
                    NonNull::from(&mut wg).cast::<c_void>(),
                    &mut size,
                )
            };
            if st == 0 {
                ctx.workgroup.store(wg, Ordering::Release);
            } else {
                eprintln!(
                    "nxkit-audio: the output unit has no os_workgroup (status {st}), the audio thread stays outside it"
                );
            }
            // SAFETY: the unit is initialized.
            #[cfg(target_os = "macos")]
            check("start the output unit", unsafe {
                AudioOutputUnitStart(unit)
            })?;
            #[cfg(target_os = "ios")]
            {
                let mut state = OUTPUT
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.0 {
                    // SAFETY: the unit is initialized and this lock serializes start/stop.
                    check("start the output unit", unsafe {
                        AudioOutputUnitStart(unit)
                    })?;
                }
                state.1 = unit as usize;
            }
            Ok(Output {
                ctx: std::ptr::from_ref(ctx),
                mode,
            })
        }

        /// How the frames reach the device, for the log.
        pub fn mode(&self) -> &'static str {
            self.mode
        }

        /// The result of `os_workgroup_join` once the first callback ran, or `None` before that.
        pub fn join_status(&self) -> Option<i32> {
            // SAFETY: `ctx` is leaked and lives until the process ends. Only the atomic is read.
            let v = unsafe { (*self.ctx).join_status.load(Ordering::Acquire) };
            if v == i32::MIN { None } else { Some(v) }
        }
    }

    /// Puts 5.1 frames on `device`: through a spatial mixer where the device has spatial audio, else through the
    /// SDK's stereo downmix. Returns the fill of the device, the mixer and the mode for the log.
    #[cfg(target_os = "macos")]
    fn route(device: u32, fill: Fill) -> (Fill, Option<Spatial>, &'static str) {
        let head = crate::settings::settings().head_tracking;
        let fill = match spatial::output_type(device) {
            Some(kind) => match Spatial::new(kind, head, fill) {
                Ok(sp) => {
                    let mode = if kind == spatial::HEADPHONES {
                        "5.1 as spatial audio for headphones"
                    } else {
                        "5.1 as spatial audio for the built-in speakers"
                    };
                    let none: Fill = Box::new(|out: &mut [f32]| out.fill(0.0));
                    return (none, Some(sp), mode);
                }
                Err((e, fill)) => {
                    eprintln!("nxkit-audio: warning: no spatial audio ({e}), 5.1 is mixed down");
                    fill
                }
            },
            None => fill,
        };
        (
            stereo_downmix(fill),
            None,
            "5.1 mixed down to stereo (the device has no spatial audio)",
        )
    }

    fn stereo_downmix(mut fill: Fill) -> Fill {
        let mut six = vec![0.0f32; spatial::MAX_FRAMES * 6].into_boxed_slice();
        let down: Fill = Box::new(move |out: &mut [f32]| {
            let n = out.len() / 2;
            if n * 6 > six.len() {
                out.fill(0.0);
                return;
            }
            fill(&mut six[..n * 6]);
            crate::chans::downmix(&six[..n * 6], out);
        });
        down
    }

    // The mixer owns one output for the process lifetime, as on macOS.
    // Swift owns AVAudioSession and sends effective foreground/interruption state.
    #[cfg(target_os = "ios")]
    static OUTPUT: std::sync::Mutex<(bool, usize)> = std::sync::Mutex::new((false, 0));

    /// Start or stop the process-lifetime RemoteIO unit.
    #[cfg(target_os = "ios")]
    pub fn set_active(active: bool) -> Result<(), String> {
        let mut state = OUTPUT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.0 = active;
        if state.1 == 0 {
            return Ok(());
        }
        let unit = state.1 as AudioComponentInstance;
        // SAFETY: the process-lifetime unit is initialized; the lock serializes state changes.
        let status = unsafe {
            if active {
                AudioOutputUnitStart(unit)
            } else {
                objc2_audio_toolbox::AudioOutputUnitStop(unit)
            }
        };
        check("change output activity", status)
    }

    /// The layout of `AURenderCallbackStruct`.
    #[repr(C)]
    struct CallbackStruct {
        proc_: *const c_void,
        refcon: *mut c_void,
    }
}

#[cfg(not(target_vendor = "apple"))]
mod imp {
    use super::Fill;

    /// There is no output device on this platform.
    pub struct Output;

    impl Output {
        /// Always fails: this platform has no output unit.
        pub fn start(_fill: Fill, _width: usize) -> Result<Output, String> {
            Err("this platform has no audio output unit".to_string())
        }

        /// Never started.
        pub fn mode(&self) -> &'static str {
            "none"
        }

        /// Never joined.
        pub fn join_status(&self) -> Option<i32> {
            None
        }
    }
}

pub use imp::Output;

#[cfg(target_os = "ios")]
pub use imp::set_active;
