//! A Metal GPU trace capture of the device, for Xcode (Pixel History and the other GPU tools).
//!
//! The capture takes every command buffer that any queue of the device commits between [`start`] and [`stop`]. Metal
//! makes a capture only in a process that started with `MTL_CAPTURE_ENABLED=1` in its environment. The host sets that
//! before the first device exists.

use std::path::Path;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_foundation::{NSString, NSURL};
use objc2_metal::{MTLCaptureDescriptor, MTLCaptureDestination, MTLCaptureManager};

use crate::device::Context;

/// True when the process can write a GPU trace document.
pub fn gpu_trace_supported() -> bool {
    // SAFETY: the shared capture manager is available on any thread.
    let m = unsafe { MTLCaptureManager::sharedCaptureManager() };
    m.supportsDestination(MTLCaptureDestination::GPUTraceDocument)
}

/// True while a capture runs.
pub fn gpu_trace_running() -> bool {
    // SAFETY: as above.
    unsafe { MTLCaptureManager::sharedCaptureManager() }.isCapturing()
}

/// Starts a capture of the whole device into the document `path`. The file must not exist.
pub fn gpu_trace_start(ctx: &Context, path: &Path) -> Result<(), String> {
    // SAFETY: the shared capture manager is available on any thread.
    let m = unsafe { MTLCaptureManager::sharedCaptureManager() };
    if !m.supportsDestination(MTLCaptureDestination::GPUTraceDocument) {
        return Err(
            "GPU trace capture is not supported: the process started without MTL_CAPTURE_ENABLED=1 \
             (start the game with NXKIT_GPU_CAPTURE=1) or the system refuses it"
                .to_owned(),
        );
    }
    if m.isCapturing() {
        return Err("a capture runs already".to_owned());
    }
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.display().to_string()));
    let desc = MTLCaptureDescriptor::new();
    let device: &ProtocolObject<dyn objc2_metal::MTLDevice> = ctx.device();
    let object: &AnyObject = device.as_ref();
    // SAFETY: the capture object is the Metal device, which the descriptor accepts.
    unsafe { desc.setCaptureObject(Some(object)) };
    desc.setDestination(MTLCaptureDestination::GPUTraceDocument);
    desc.setOutputURL(Some(&url));
    m.startCaptureWithDescriptor_error(&desc)
        .map_err(|e| format!("the capture did not start: {}", e.localizedDescription()))
}

/// Ends the capture. The caller waited for the GPU work of the frames before.
pub fn gpu_trace_stop() {
    // SAFETY: as above.
    let m: Retained<MTLCaptureManager> = unsafe { MTLCaptureManager::sharedCaptureManager() };
    if m.isCapturing() {
        m.stopCapture();
    }
}
