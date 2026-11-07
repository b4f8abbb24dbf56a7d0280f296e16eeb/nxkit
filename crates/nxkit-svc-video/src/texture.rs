//! Metal textures for a decoded picture, without a copy (R20).
//!
//! VideoToolbox fills `CVPixelBuffer`s that are backed by an IOSurface. `CVMetalTextureCache` maps each plane of such a
//! buffer as an `MTLTexture` on the same memory. The host (`nxkit-nvn` or `nxkit-metal`) owns the cache and the device.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::CFRetained;
use objc2_core_video::{CVImageBuffer, CVMetalTexture, CVMetalTextureCache, CVPixelBuffer};
use objc2_metal::{MTLDevice, MTLPixelFormat, MTLTexture};

use crate::frame::{Frame, Pixels};

/// The two textures of an NV12 picture.
pub struct Planes {
    /// The luma plane, `R8Unorm`, the size of the picture.
    pub luma: Retained<ProtocolObject<dyn MTLTexture>>,
    /// The chroma plane, `RG8Unorm`, half the size of the picture in each direction. Cb is red and Cr is green.
    pub chroma: Retained<ProtocolObject<dyn MTLTexture>>,
    // The textures stay valid while these are alive.
    _luma_ref: CFRetained<CVMetalTexture>,
    _chroma_ref: CFRetained<CVMetalTexture>,
}

/// A `CVMetalTextureCache` on one Metal device.
pub struct TextureCache {
    cache: CFRetained<CVMetalTextureCache>,
}

impl TextureCache {
    /// A cache for textures on `device`.
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Result<TextureCache, String> {
        let mut cache = None;
        // SAFETY: `device` is live for the call and `cache` is `None` as the function needs.
        unsafe { CVMetalTextureCache::new(None, None, device, None, &mut cache) }
            .map_err(|e| format!("CVMetalTextureCacheCreate failed: {e:?}"))?;
        let cache = cache.ok_or_else(|| "CVMetalTextureCacheCreate gave no cache".to_string())?;
        Ok(TextureCache { cache })
    }

    /// The textures of the planes of `frame`. A picture that is in memory and not in a pixel buffer has no textures.
    /// The textures show the pixel buffer as it is: the caller keeps `frame` alive until the GPU is done with them.
    pub fn planes(&self, frame: &Frame) -> Result<Planes, String> {
        let Pixels::Buffer(buffer) = &frame.pixels else {
            return Err("the picture is in memory, not in a pixel buffer".to_string());
        };
        let pb: &CVPixelBuffer = &buffer.0;
        let image: &CVImageBuffer = pb;
        let (luma, luma_ref) = self.plane(image, MTLPixelFormat::R8Unorm, pb.width_of_plane(0), pb.height_of_plane(0), 0)?;
        let (chroma, chroma_ref) = self.plane(image, MTLPixelFormat::RG8Unorm, pb.width_of_plane(1), pb.height_of_plane(1), 1)?;
        Ok(Planes { luma, chroma, _luma_ref: luma_ref, _chroma_ref: chroma_ref })
    }

    fn plane(
        &self,
        image: &CVImageBuffer,
        format: MTLPixelFormat,
        width: usize,
        height: usize,
        index: usize,
    ) -> Result<(Retained<ProtocolObject<dyn MTLTexture>>, CFRetained<CVMetalTexture>), String> {
        let mut out = None;
        // SAFETY: `image` is a live pixel buffer with an IOSurface, and `out` is `None` as the function needs.
        unsafe { self.cache.texture_from_image(None, image, None, format, width, height, index, &mut out) }
            .map_err(|e| format!("CVMetalTextureCacheCreateTextureFromImage failed for plane {index}: {e:?}"))?;
        let holder = out.ok_or_else(|| format!("plane {index} gave no CVMetalTexture"))?;
        let texture = holder.texture().ok_or_else(|| format!("plane {index} gave no MTLTexture"))?;
        Ok((texture, holder))
    }

    /// Releases the textures that the cache holds for pictures that are gone. Call it once per frame.
    pub fn flush(&self) {
        self.cache.flush(0);
    }
}
