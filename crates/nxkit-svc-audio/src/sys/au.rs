//! The output unit (AUHAL) on the default output device.
//!
//! The unit calls one closure on the device's real-time thread. The first call joins that thread to the `os_workgroup` of
//! the device, so the scheduler treats it as part of the device's deadline (R20).

#![allow(unsafe_code)]

/// The closure the device thread runs. It fills an interleaved stereo `f32` buffer with silence or sound.
pub type Fill = Box<dyn FnMut(&mut [f32]) + Send>;

#[cfg(target_vendor = "apple")]
mod imp {
    use super::Fill;
    use objc2_audio_toolbox::{
        AudioComponentDescription, AudioComponentFindNext, AudioComponentInstance, AudioComponentInstanceNew,
        AudioOutputUnitStart, AudioUnitGetProperty, AudioUnitInitialize, AudioUnitSetProperty,
    };
    use objc2_core_audio::{AudioObjectGetPropertyData, AudioObjectPropertyAddress};
    use objc2_core_audio_types::{AudioBufferList, AudioStreamBasicDescription};
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};

    const SCOPE_GLOBAL: u32 = 0;
    const SCOPE_INPUT: u32 = 1;
    const PROP_STREAM_FORMAT: u32 = 8;
    const PROP_SET_RENDER_CALLBACK: u32 = 23;
    const PROP_CURRENT_DEVICE: u32 = 2000;
    const PROP_OS_WORKGROUP: u32 = 2015;
    const FORMAT_LINEAR_PCM: u32 = 0x6c70_636d;
    const FLAGS_FLOAT_PACKED: u32 = 1 | 8;
    const SYSTEM_OBJECT: u32 = 1;
    const SELECTOR_DEFAULT_OUTPUT: u32 = 0x644f_7574;
    const SCOPE_HAL_GLOBAL: u32 = 0x676c_6f62;

    unsafe extern "C" {
        fn os_workgroup_join(wg: *mut c_void, token: *mut u64) -> i32;
    }

    /// The callback context. It is boxed and never freed, because the unit runs until the process ends.
    struct Ctx {
        fill: Fill,
        workgroup: AtomicPtr<c_void>,
        join_status: AtomicI32,
        joined: bool,
        token: [u64; 8],
    }

    /// The running output unit.
    pub struct Output {
        ctx: *const Ctx,
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
        // SAFETY: the unit hands us a valid list that lives during this call.
        let list = unsafe { &*data };
        if list.mNumberBuffers == 0 {
            return 0;
        }
        let buf = &list.mBuffers[0];
        let len = (buf.mDataByteSize / 4) as usize;
        if buf.mData.is_null() || len == 0 || len % 2 != 0 {
            return 0;
        }
        // SAFETY: `mData` points at `mDataByteSize` bytes of `f32` that this call owns until it returns.
        let out = unsafe { std::slice::from_raw_parts_mut(buf.mData.cast::<f32>(), len) };
        (ctx.fill)(out);
        0
    }

    fn check(what: &str, status: i32) -> Result<(), String> {
        if status == 0 { Ok(()) } else { Err(format!("{what} failed with status {status}")) }
    }

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
        /// Opens the default output device at 48 kHz stereo `f32`, joins its workgroup and starts it.
        pub fn start(fill: Fill) -> Result<Output, String> {
            let device = default_device()?;
            let desc = AudioComponentDescription {
                componentType: 0x6175_6f75,
                componentSubType: 0x6168_616c,
                componentManufacturer: 0x6170_706c,
                componentFlags: 0,
                componentFlagsMask: 0,
            };
            // SAFETY: a null start component and a valid description.
            let comp = unsafe { AudioComponentFindNext(std::ptr::null_mut(), &desc) };
            if comp.is_null() {
                return Err("no HAL output component".to_string());
            }
            let mut unit: AudioComponentInstance = std::ptr::null_mut();
            // SAFETY: `comp` is a live component and `unit` receives the instance.
            check("create the output unit", unsafe { AudioComponentInstanceNew(comp, &mut unit) })?;

            // SAFETY: `device` is 4 bytes, as `CurrentDevice` needs.
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
            let fmt = AudioStreamBasicDescription {
                mSampleRate: 48000.0,
                mFormatID: FORMAT_LINEAR_PCM,
                mFormatFlags: FLAGS_FLOAT_PACKED,
                mBytesPerPacket: 8,
                mFramesPerPacket: 1,
                mBytesPerFrame: 8,
                mChannelsPerFrame: 2,
                mBitsPerChannel: 32,
                mReserved: 0,
            };
            let fmt_size = u32::try_from(std::mem::size_of::<AudioStreamBasicDescription>()).unwrap_or(40);
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
                workgroup: AtomicPtr::new(std::ptr::null_mut()),
                join_status: AtomicI32::new(i32::MIN),
                joined: false,
                token: [0; 8],
            }));
            let cb = CallbackStruct {
                proc_: render as *const c_void,
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
            check("initialize the output unit", unsafe { AudioUnitInitialize(unit) })?;

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
                eprintln!("nxkit-audio: the output unit has no os_workgroup (status {st}), the audio thread stays outside it");
            }
            // SAFETY: the unit is initialized.
            check("start the output unit", unsafe { AudioOutputUnitStart(unit) })?;
            Ok(Output { ctx: std::ptr::from_ref(ctx) })
        }

        /// The result of `os_workgroup_join` once the first callback ran, or `None` before that.
        pub fn join_status(&self) -> Option<i32> {
            // SAFETY: `ctx` is leaked and lives until the process ends. Only the atomic is read.
            let v = unsafe { (*self.ctx).join_status.load(Ordering::Acquire) };
            if v == i32::MIN { None } else { Some(v) }
        }
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
        pub fn start(_fill: Fill) -> Result<Output, String> {
            Err("this platform has no audio output unit".to_string())
        }

        /// Never joined.
        pub fn join_status(&self) -> Option<i32> {
            None
        }
    }
}

pub use imp::Output;
