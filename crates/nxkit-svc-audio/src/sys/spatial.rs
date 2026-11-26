//! Spatial audio (`NXKIT_SPATIAL_AUDIO=1`): Apple's AUSpatialMixer between the mixer's 5.1 frames and the output unit.
//!
//! The mixer gets the six channels as an ambience bed (`kSpatialMixerSourceMode_AmbienceBed`) in the layout
//! `kAudioChannelLayoutTag_MPEG_5_1_A`, which is the channel order of the SDK, and renders them for the hardware
//! (`kSpatializationAlgorithm_UseOutputType`): the Mac's built-in speakers, or headphones with the personalized HRTF of
//! the player and, with `NXKIT_HEAD_TRACKING=1`, the head tracking of AirPods. The output unit pulls the mixer in its
//! callback, and the mixer pulls the frames in its input callback on the same thread.

use super::au::Fill;
use objc2_audio_toolbox::{
    AudioComponentDescription, AudioComponentFindNext, AudioComponentInstance,
    AudioComponentInstanceDispose, AudioUnitInitialize, AudioUnitRenderActionFlags,
    AudioUnitSetProperty,
};
#[cfg(target_os = "macos")]
use objc2_core_audio::{AudioObjectGetPropertyData, AudioObjectPropertyAddress};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp,
};
use std::ffi::c_void;
use std::ptr::NonNull;

const SCOPE_GLOBAL: u32 = 0;
const SCOPE_INPUT: u32 = 1;
const SCOPE_OUTPUT: u32 = 2;
const PROP_STREAM_FORMAT: u32 = 8;
const PROP_ELEMENT_COUNT: u32 = 11;
const PROP_MAX_FRAMES: u32 = 14;
const PROP_CHANNEL_LAYOUT: u32 = 19;
const PROP_SET_RENDER_CALLBACK: u32 = 23;
const PROP_ALGORITHM: u32 = 3000;
const PROP_SOURCE_MODE: u32 = 3005;
const PROP_OUTPUT_TYPE: u32 = 3100;
const PROP_HEAD_TRACKING: u32 = 3111;
const PROP_PERSONAL_HRTF: u32 = 3113;
const ALGORITHM_USE_OUTPUT_TYPE: u32 = 7;
const SOURCE_AMBIENCE_BED: u32 = 3;
const HRTF_AUTO: u32 = 2;
const LAYOUT_5_1: u32 = (121 << 16) | 6;
const LAYOUT_STEREO: u32 = (101 << 16) | 2;

/// The most frames of one callback. A longer request plays silence.
pub const MAX_FRAMES: usize = 4096;

/// `kSpatialMixerOutputType_Headphones`.
pub const HEADPHONES: u32 = 1;
/// `kSpatialMixerOutputType_BuiltInSpeakers`.
pub const SPEAKERS: u32 = 2;

/// 48 kHz linear PCM `f32` with `ch` channels, interleaved or one buffer per channel.
pub fn format(ch: u32, interleaved: bool) -> AudioStreamBasicDescription {
    let frame = if interleaved { 4 * ch } else { 4 };
    AudioStreamBasicDescription {
        mSampleRate: 48000.0,
        mFormatID: 0x6c70_636d,
        mFormatFlags: 1 | 8 | if interleaved { 0 } else { 32 },
        mBytesPerPacket: frame,
        mFramesPerPacket: 1,
        mBytesPerFrame: frame,
        mChannelsPerFrame: ch,
        mBitsPerChannel: 32,
        mReserved: 0,
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
fn device_u32(device: u32, selector: u32, scope: u32) -> Option<u32> {
    let addr = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
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

/// The output type of the device for the spatial mixer, or `None` for hardware that it does not render for (HDMI, USB,
/// AirPlay): those get the SDK's stereo downmix.
#[cfg(target_os = "macos")]
pub fn output_type(device: u32) -> Option<u32> {
    // `kAudioDevicePropertyTransportType`, `kAudioDevicePropertyDataSource`, the global and the output scope.
    let transport = device_u32(device, 0x7472_616e, 0x676c_6f62)?;
    match &transport.to_be_bytes() {
        b"blue" | b"blea" => Some(HEADPHONES),
        b"bltn" => match device_u32(device, 0x7373_7263, 0x6f75_7470).map(u32::to_be_bytes) {
            Some(s) if &s == b"hdpn" => Some(HEADPHONES),
            _ => Some(SPEAKERS),
        },
        _ => None,
    }
}

/// What the mixer's input callback needs: the frames and room to split them into channels.
struct In {
    fill: Fill,
    scratch: Box<[f32]>,
}

unsafe extern "C-unwind" fn pull(
    refcon: NonNull<c_void>,
    _flags: NonNull<AudioUnitRenderActionFlags>,
    _stamp: NonNull<AudioTimeStamp>,
    _bus: u32,
    frames: u32,
    data: *mut AudioBufferList,
) -> i32 {
    if data.is_null() {
        return 0;
    }
    // SAFETY: `refcon` is the leaked `In` of `Spatial::new`, which only this callback touches.
    let ctx = unsafe { &mut *refcon.as_ptr().cast::<In>() };
    // SAFETY: the unit hands us a list of `mNumberBuffers` buffers that lives during this call.
    let bufs = unsafe {
        let n = (*data).mNumberBuffers as usize;
        std::slice::from_raw_parts_mut(
            std::ptr::addr_of_mut!((*data).mBuffers).cast::<AudioBuffer>(),
            n,
        )
    };
    let n = frames as usize;
    let mut outs: [&mut [f32]; 6] = Default::default();
    for (o, b) in outs.iter_mut().zip(bufs.iter()) {
        if !b.mData.is_null() && (b.mDataByteSize / 4) as usize >= n {
            // SAFETY: `mData` points at `mDataByteSize` bytes of `f32` that this call owns until it returns.
            *o = unsafe { std::slice::from_raw_parts_mut(b.mData.cast::<f32>(), n) };
        }
    }
    if n * 6 > ctx.scratch.len() || bufs.len() != 6 {
        outs.iter_mut().for_each(|o| o.fill(0.0));
        return 0;
    }
    let src = &mut ctx.scratch[..n * 6];
    (ctx.fill)(src);
    for (j, o) in outs.iter_mut().enumerate() {
        for (i, v) in o.iter_mut().enumerate() {
            *v = src[6 * i + j];
        }
    }
    0
}

/// A running spatial mixer. It lives until the process ends.
pub struct Spatial {
    /// The mixer, which the output unit renders bus 0 of.
    pub unit: AudioComponentInstance,
}

impl Spatial {
    /// Creates the mixer for the output type `kind` and gives it `fill`. On an error the caller gets `fill` back.
    pub fn new(kind: u32, head_tracking: bool, fill: Fill) -> Result<Spatial, (String, Fill)> {
        let desc = AudioComponentDescription {
            componentType: 0x6175_6d78,
            componentSubType: 0x3364_656d,
            componentManufacturer: 0x6170_706c,
            componentFlags: 0,
            componentFlagsMask: 0,
        };
        // SAFETY: a null start component and a valid description.
        let comp = unsafe { AudioComponentFindNext(std::ptr::null_mut(), &desc) };
        if comp.is_null() {
            return Err(("there is no AUSpatialMixer".to_string(), fill));
        }
        let mut unit: AudioComponentInstance = std::ptr::null_mut();
        // SAFETY: `comp` is a live component and `unit` receives the instance.
        if let Err(e) = check("create the spatial mixer", unsafe {
            objc2_audio_toolbox::AudioComponentInstanceNew(comp, &mut unit)
        }) {
            return Err((e, fill));
        }
        let ctx: *mut In = Box::into_raw(Box::new(In {
            fill,
            scratch: vec![0.0; MAX_FRAMES * 6].into_boxed_slice(),
        }));
        match configure(unit, kind, head_tracking, ctx) {
            Ok(()) => Ok(Spatial { unit }),
            Err(e) => {
                // SAFETY: the unit failed to start, so nothing calls into `ctx` any more once it is disposed.
                let back = unsafe {
                    AudioComponentInstanceDispose(unit);
                    Box::from_raw(ctx)
                };
                Err((e, back.fill))
            }
        }
    }
}

fn set<T>(unit: AudioComponentInstance, id: u32, scope: u32, v: &T) -> i32 {
    let size = u32::try_from(std::mem::size_of::<T>()).unwrap_or(0);
    // SAFETY: `v` is a complete value of the type the property takes, of `size` bytes.
    unsafe {
        AudioUnitSetProperty(
            unit,
            id,
            scope,
            0,
            std::ptr::from_ref(v).cast::<c_void>(),
            size,
        )
    }
}

/// `AudioChannelLayout` with a tag and no channel descriptions.
#[repr(C)]
struct Layout {
    tag: u32,
    bitmap: u32,
    descriptions: u32,
    room: [u32; 5],
}

fn layout(tag: u32) -> Layout {
    Layout {
        tag,
        bitmap: 0,
        descriptions: 0,
        room: [0; 5],
    }
}

/// The layout of `AURenderCallbackStruct`.
#[repr(C)]
struct Callback {
    proc_: *const c_void,
    refcon: *mut c_void,
}

fn configure(
    unit: AudioComponentInstance,
    kind: u32,
    head: bool,
    ctx: *mut In,
) -> Result<(), String> {
    check(
        "set one input bus",
        set(unit, PROP_ELEMENT_COUNT, SCOPE_INPUT, &1u32),
    )?;
    check(
        "set the slice size",
        set(unit, PROP_MAX_FRAMES, SCOPE_GLOBAL, &(MAX_FRAMES as u32)),
    )?;
    check(
        "set the 5.1 input format",
        set(unit, PROP_STREAM_FORMAT, SCOPE_INPUT, &format(6, false)),
    )?;
    check(
        "set the stereo output format",
        set(unit, PROP_STREAM_FORMAT, SCOPE_OUTPUT, &format(2, false)),
    )?;
    check(
        "set the 5.1 layout",
        set(unit, PROP_CHANNEL_LAYOUT, SCOPE_INPUT, &layout(LAYOUT_5_1)),
    )?;
    let st = set(
        unit,
        PROP_CHANNEL_LAYOUT,
        SCOPE_OUTPUT,
        &layout(LAYOUT_STEREO),
    );
    if st != 0 {
        eprintln!("nxkit-audio: the spatial mixer keeps its own output layout (status {st})");
    }
    check(
        "set the algorithm",
        set(
            unit,
            PROP_ALGORITHM,
            SCOPE_INPUT,
            &ALGORITHM_USE_OUTPUT_TYPE,
        ),
    )?;
    check(
        "set the ambience bed",
        set(unit, PROP_SOURCE_MODE, SCOPE_INPUT, &SOURCE_AMBIENCE_BED),
    )?;
    check(
        "set the output type",
        set(unit, PROP_OUTPUT_TYPE, SCOPE_GLOBAL, &kind),
    )?;
    if kind == HEADPHONES {
        let st = set(unit, PROP_PERSONAL_HRTF, SCOPE_GLOBAL, &HRTF_AUTO);
        if st != 0 {
            eprintln!("nxkit-audio: the spatial mixer has no personalized HRTF (status {st})");
        }
        if head {
            let st = set(unit, PROP_HEAD_TRACKING, SCOPE_GLOBAL, &1u32);
            if st == 0 {
                eprintln!(
                    "nxkit-audio: head tracking is on (AirPods with motion sensors follow the head)"
                );
            } else {
                eprintln!("nxkit-audio: warning: head tracking did not turn on (status {st})");
            }
        }
    }
    let cb = Callback {
        proc_: pull as *const c_void,
        refcon: ctx.cast::<c_void>(),
    };
    check(
        "set the input callback",
        set(unit, PROP_SET_RENDER_CALLBACK, SCOPE_INPUT, &cb),
    )?;
    // SAFETY: the unit is fully configured.
    check("initialize the spatial mixer", unsafe {
        AudioUnitInitialize(unit)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_audio_toolbox::AudioUnitRender;

    /// `AudioBufferList` with two buffers.
    #[repr(C)]
    struct Two {
        n: u32,
        bufs: [AudioBuffer; 2],
    }

    /// Renders 512 frames of a tone on the 5.1 channel `ch` through a mixer for `kind`, and returns left and right.
    fn render(kind: u32, ch: usize) -> (Vec<f32>, Vec<f32>) {
        let mut t = 0usize;
        let fill: Fill = Box::new(move |out: &mut [f32]| {
            for f in out.as_chunks_mut::<6>().0 {
                f.fill(0.0);
                f[ch] = (t as f32 * 0.05).sin() * 0.5;
                t += 1;
            }
        });
        let sp = match Spatial::new(kind, false, fill) {
            Ok(sp) => sp,
            Err((e, _)) => panic!("{e}"),
        };
        let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        let buf = |v: &mut Vec<f32>| AudioBuffer {
            mNumberChannels: 1,
            mDataByteSize: 512 * 4,
            mData: v.as_mut_ptr().cast(),
        };
        let mut list = Two {
            n: 2,
            bufs: [buf(&mut l), buf(&mut r)],
        };
        // SAFETY: a zeroed time stamp is a valid value. The sample time is marked valid.
        let mut stamp: AudioTimeStamp = unsafe { std::mem::zeroed() };
        stamp.mFlags = objc2_core_audio_types::AudioTimeStampFlags(1);
        for _ in 0..4 {
            let mut flags = AudioUnitRenderActionFlags(0);
            // SAFETY: the list has two buffers of 512 frames that live during the call.
            let st = unsafe {
                AudioUnitRender(
                    sp.unit,
                    Some(&mut flags),
                    &stamp,
                    0,
                    512,
                    NonNull::from(&mut list).cast(),
                )
            };
            assert_eq!(st, 0, "render");
            stamp.mSampleTime += 512.0;
        }
        (l, r)
    }

    fn energy(v: &[f32]) -> f32 {
        v.iter().map(|x| x * x).sum()
    }

    #[test]
    fn the_mixer_renders_the_bed_for_the_speakers_and_headphones() {
        for kind in [SPEAKERS, HEADPHONES] {
            for ch in [0, 5] {
                let (l, r) = render(kind, ch);
                assert!(
                    energy(&l) + energy(&r) > 1.0,
                    "{kind}: channel {ch} is silent"
                );
            }
        }
    }

    /// Where Apple's mixer places a channel depends on the Mac: its speakers and the HRTF it has (a CI runner has no
    /// personalized HRTF, and its headphone rendering put front left on the right). Run it on a Mac with
    /// `cargo test -p nxkit-svc-audio -- --ignored`.
    #[test]
    #[ignore = "depends on the Mac's speakers and HRTF"]
    fn the_mixer_places_the_channels_for_the_speakers_and_headphones() {
        for kind in [SPEAKERS, HEADPHONES] {
            let (l, r) = render(kind, 0);
            assert!(
                energy(&l) > energy(&r),
                "{kind}: front left is not on the left"
            );
            let (l, r) = render(kind, 5);
            assert!(
                energy(&r) > energy(&l),
                "{kind}: back right is not on the right"
            );
        }
    }
}
