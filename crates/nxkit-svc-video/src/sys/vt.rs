//! The VideoToolbox side of the decoder: a VP9 or AV1 decompression session that fills `CVPixelBuffer`s which Metal can
//! use.

use std::ffi::c_void;
use std::ptr;
use std::sync::{Mutex, Once};

use objc2_core_foundation::{CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType, Type, kCFAllocatorNull};
use objc2_core_media::{
    CMBlockBuffer, CMSampleBuffer, CMTime, CMVideoFormatDescription, kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms,
    kCMVideoCodecType_AV1, kCMVideoCodecType_VP9,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBuffer, CVPixelBufferLockFlags, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord, VTDecompressionSession, VTIsHardwareDecodeSupported,
    VTRegisterSupplementalVideoDecoderIfAvailable,
};

use crate::av1::Sequence;
use crate::vp9::KeyFrame;

/// A `CVPixelBuffer` that a decoder filled, with its planes in NV12.
pub(crate) struct PixelBuffer(pub(crate) CFRetained<CVPixelBuffer>);

// SAFETY: a CVPixelBuffer is a reference counted CoreFoundation object. CoreVideo documents it as usable from any
// thread, and this crate only reads it, under a base address lock.
unsafe impl Send for PixelBuffer {}
// SAFETY: as for `Send`. A read lock on the base address may be held by several threads at once.
unsafe impl Sync for PixelBuffer {}

/// Holds the read lock of the base address of a pixel buffer.
struct Locked<'a>(&'a CVPixelBuffer);

impl<'a> Locked<'a> {
    fn new(buffer: &'a CVPixelBuffer) -> Result<Locked<'a>, String> {
        // SAFETY: the buffer is a live pixel buffer. The lock is released in `drop`.
        unsafe { buffer.lock_base_address(CVPixelBufferLockFlags::ReadOnly) }
            .map_err(|e| format!("CVPixelBufferLockBaseAddress failed: {e:?}"))?;
        Ok(Locked(buffer))
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: `new` locked this buffer and nothing else unlocks it.
        let _ = unsafe { self.0.unlock_base_address(CVPixelBufferLockFlags::ReadOnly) };
    }
}

impl PixelBuffer {
    /// The width and the height of the picture in pixels.
    pub(crate) fn size(&self) -> (u32, u32) {
        let pb: &CVPixelBuffer = &self.0;
        (pb.width() as u32, pb.height() as u32)
    }

    /// Calls `f` with the luma plane, its stride, the chroma plane and its stride while the buffer is locked.
    pub(crate) fn with_planes<R>(&self, f: impl FnOnce(&[u8], usize, &[u8], usize) -> R) -> Result<R, String> {
        let pb: &CVPixelBuffer = &self.0;
        if pb.plane_count() != 2 {
            return Err(format!("the pixel buffer has {} planes, NV12 has 2", pb.plane_count()));
        }
        let _lock = Locked::new(pb)?;
        let (ys, uvs) = (pb.bytes_per_row_of_plane(0), pb.bytes_per_row_of_plane(1));
        let (yh, uvh) = (pb.height_of_plane(0), pb.height_of_plane(1));
        let (yp, uvp) = (pb.base_address_of_plane(0), pb.base_address_of_plane(1));
        if yp.is_null() || uvp.is_null() {
            return Err("the pixel buffer has no base address".to_string());
        }
        // SAFETY: the buffer is locked, so each plane address is valid for `bytes per row * rows` bytes until `_lock`
        // drops, and nothing writes to a buffer that the decoder has handed out.
        let (y, uv) = unsafe {
            (std::slice::from_raw_parts(yp as *const u8, ys * yh), std::slice::from_raw_parts(uvp as *const u8, uvs * uvh))
        };
        Ok(f(y, ys, uv, uvs))
    }
}

struct Out {
    status: i32,
    buffer: Option<PixelBuffer>,
}

/// The callback of the session. With the asynchronous flag clear, it runs before `decode_frame` returns.
unsafe extern "C-unwind" fn on_frame(
    refcon: *mut c_void,
    _frame_refcon: *mut c_void,
    status: i32,
    _info: VTDecodeInfoFlags,
    image: Option<&CVImageBuffer>,
    _pts: CMTime,
    _duration: CMTime,
) {
    // SAFETY: `refcon` is the `Mutex<Out>` that the `Session` boxed. It is not freed before the session is
    // invalidated, and the session is invalidated before the box is dropped.
    let out = unsafe { &*(refcon as *const Mutex<Out>) };
    let Ok(mut out) = out.lock() else { return };
    out.status = status;
    out.buffer = image.and_then(|i| i.downcast_ref::<CVPixelBuffer>()).map(|p| PixelBuffer(p.retain()));
}

fn register_vp9() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: a plain call with a codec type. VP9 is a supplemental decoder that a process opts into.
        unsafe { VTRegisterSupplementalVideoDecoderIfAvailable(kCMVideoCodecType_VP9) };
    });
}

/// Whether VideoToolbox says it decodes AV1 in hardware on this Mac (Apple M3 and A17 Pro on). Its AV1 decoder is
/// hardware only.
pub(crate) fn av1_hardware() -> bool {
    // SAFETY: a plain query with a codec type.
    unsafe { VTIsHardwareDecodeSupported(kCMVideoCodecType_AV1) }
}

/// A decompression session for one stream: VP9 or AV1, 8-bit 4:2:0.
pub(crate) struct Session {
    name: &'static str,
    session: CFRetained<VTDecompressionSession>,
    format: CFRetained<CMVideoFormatDescription>,
    out: Box<Mutex<Out>>,
    pixel_format: u32,
}

// SAFETY: a VTDecompressionSession and a format description are CoreFoundation objects that VideoToolbox documents as
// usable from any thread. The output slot sits behind a mutex, and `decode` takes `&mut self`.
unsafe impl Send for Session {}
// SAFETY: as for `Send`. Nothing is reachable through a shared reference except the mutex.
unsafe impl Sync for Session {}

impl Session {
    /// Opens a session for a VP9 stream that starts with `key`. The stream must be 8-bit 4:2:0.
    pub(crate) fn vp9(key: &KeyFrame) -> Result<Session, String> {
        if !key.is_nv12() {
            return Err(format!(
                "VP9 profile {} at {} bits with subsampling {}/{} has no NV12 output",
                key.profile, key.bit_depth, key.subsampling_x as u8, key.subsampling_y as u8
            ));
        }
        register_vp9();
        Session::open("VP9", kCMVideoCodecType_VP9, "vpcC", &key.vpcc(), key.width, key.height, key.full_range)
    }

    /// Opens a session for an AV1 stream with the sequence header `seq`. The stream must be 8-bit 4:2:0, and this Mac
    /// must decode AV1 in hardware.
    pub(crate) fn av1(seq: &Sequence) -> Result<Session, String> {
        if !seq.is_nv12() {
            return Err(format!(
                "AV1 profile {} at {} bits with subsampling {}/{} has no NV12 output",
                seq.profile, seq.bit_depth, seq.subsampling_x as u8, seq.subsampling_y as u8
            ));
        }
        Session::open("AV1", kCMVideoCodecType_AV1, "av1C", &seq.av1c(), seq.max_width, seq.max_height, seq.full_range)
    }

    fn open(
        name: &'static str,
        codec: u32,
        atom_name: &'static str,
        atom: &[u8],
        width: u32,
        height: u32,
        full_range: bool,
    ) -> Result<Session, String> {
        let (width_i, height_i) = (i32::try_from(width), i32::try_from(height));
        let (Ok(width_i), Ok(height_i)) = (width_i, height_i) else {
            return Err(format!("the size {width}x{height} is out of range"));
        };

        let atom = CFData::from_bytes(atom);
        let atom_key = CFString::from_str(atom_name);
        let atom_value: &CFType = &atom;
        let atoms = CFDictionary::<CFString, CFType>::from_slices(&[&*atom_key], &[atom_value]);
        // SAFETY: reads a constant that CoreMedia exports.
        let extensions_key = unsafe { kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms };
        let atoms_value: &CFType = &atoms;
        let extensions = CFDictionary::<CFString, CFType>::from_slices(&[extensions_key], &[atoms_value]);
        let mut format = None;
        // SAFETY: every reference is live for the call and `format` is `None` as the function needs.
        let status = unsafe { CMVideoFormatDescription::new(None, codec, width_i, height_i, Some(&extensions), &mut format) };
        let Some(format) = format.filter(|_| status == 0) else {
            return Err(format!("CMVideoFormatDescriptionCreate failed with OSStatus {status}"));
        };

        let pixel_format = if full_range {
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
        } else {
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        };
        let number = CFNumber::new_u32(pixel_format);
        let surface = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        // SAFETY: reads constants that CoreVideo exports.
        let (k_format, k_metal, k_surface) =
            unsafe { (kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferIOSurfacePropertiesKey) };
        let values: [&CFType; 3] = [&number, CFBoolean::new(true), &surface];
        let attrs = CFDictionary::<CFString, CFType>::from_slices(&[k_format, k_metal, k_surface], &values);

        let out = Box::new(Mutex::new(Out { status: 0, buffer: None }));
        let record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(on_frame),
            decompressionOutputRefCon: &*out as *const Mutex<Out> as *mut c_void,
        };
        let mut session = None;
        // SAFETY: the format and the attributes are live for the call. The callback gets the refcon of `out`, which
        // this struct keeps alive until `drop` invalidates the session.
        let status = unsafe { VTDecompressionSession::new(None, &format, None, Some(&attrs), Some(&record), &mut session) };
        let Some(session) = session.filter(|_| status == 0) else {
            return Err(format!("VTDecompressionSessionCreate failed with OSStatus {status}"));
        };
        Ok(Session { name, session, format, out, pixel_format })
    }

    /// Decodes one packet. `Ok(None)` when the packet shows no picture, for example a frame that only updates
    /// references.
    pub(crate) fn decode(&mut self, packet: &[u8]) -> Result<Option<PixelBuffer>, String> {
        if packet.is_empty() {
            return Ok(None);
        }
        let mut data = packet.to_vec();
        let len = data.len();
        let mut block = None;
        // SAFETY: `data` outlives the block buffer, which is dropped at the end of this function. The null allocator
        // keeps CoreMedia from freeing memory it does not own.
        let status = unsafe {
            CMBlockBuffer::with_memory_block(None, data.as_mut_ptr().cast(), len, kCFAllocatorNull, None, 0, len, 0, &mut block)
        };
        let Some(block) = block.filter(|_| status == 0) else {
            return Err(format!("CMBlockBufferCreateWithMemoryBlock failed with OSStatus {status}"));
        };
        let mut sample = None;
        // SAFETY: the block buffer and the format are live. One sample of `len` bytes, no timing entries.
        let status = unsafe {
            CMSampleBuffer::new_ready(None, Some(&block), Some(&self.format), 1, 0, ptr::null(), 1, &len, &mut sample)
        };
        let Some(sample) = sample.filter(|_| status == 0) else {
            return Err(format!("CMSampleBufferCreateReady failed with OSStatus {status}"));
        };

        if let Ok(mut o) = self.out.lock() {
            o.status = 0;
            o.buffer = None;
        }
        // SAFETY: the sample is live. With no flags the decoder emits every frame before it returns.
        let status = unsafe { self.session.decode_frame(&sample, VTDecodeFrameFlags::empty(), ptr::null_mut(), None) };
        if status != 0 {
            return Err(format!("VTDecompressionSessionDecodeFrame failed with OSStatus {status}"));
        }
        let mut o = self.out.lock().map_err(|_| "the decoder callback panicked".to_string())?;
        if o.status != 0 {
            return Err(format!("the {} decoder reported OSStatus {}", self.name, o.status));
        }
        let Some(buffer) = o.buffer.take() else { return Ok(None) };
        let pb: &CVPixelBuffer = &buffer.0;
        if pb.pixel_format_type() != self.pixel_format || pb.plane_count() != 2 {
            return Err(format!(
                "the decoder gave pixel format {:#x} with {} planes, wanted {:#x} with 2",
                pb.pixel_format_type(),
                pb.plane_count(),
                self.pixel_format
            ));
        }
        Ok(Some(buffer))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the session is live and is not used after this. Invalidating it stops any call of the callback, so
        // the `out` box can be freed after.
        unsafe { self.session.invalidate() };
    }
}
