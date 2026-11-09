//! The native passes as data: the shared plan types, with the names of this network's game resources.

pub use nxkit_native_plan::*;

/// The Y plane the colour conversion reads (R8, 1920x1080).
pub const LUMA_PLANE: GameResource = GameResource::Texture(0);
/// The CbCr plane the colour conversion samples (RG8, 960x540), with the game's sampler.
pub const CHROMA_PLANE: GameResource = GameResource::Texture(1);
/// The dither texture the resampler reads (`& 0x1ff` coordinates).
pub const DITHER: GameResource = GameResource::Texture(2);
/// The push constants of the colour conversion (bank 24, 60 bytes).
pub const CONVERSION_PUSH: GameResource = GameResource::Push(0);
/// The push constants of the resampler (bank 24, 84 bytes).
pub const RESAMPLER_PUSH: GameResource = GameResource::Push(1);
/// The image the resampler writes (RGBA8, 2560x1440): the output of the whole chain.
pub const OUTPUT: GameResource = GameResource::Output;
/// The RGBA8 picture of the conversion.
pub const PICTURE_RGBA8: GameResource = GameResource::Intermediate { index: 0 };
/// The RGB10A2 picture of layer 12.
pub const PICTURE_RGB10A2: GameResource = GameResource::Intermediate { index: 1 };

/// The Neural Engine's input tensor `x` (R16Float, 1920 x 4320), written by the GPU. Bound from the slot of the ring.
pub const ANE_X: GameResource = GameResource::Image(0);
/// The Neural Engine's output `final` (R16Float, 1920 x 3240).
pub const ANE_FINAL: GameResource = GameResource::Image(1);
/// The Neural Engine's output `delta` (R16Float, 2560 x 1440).
pub const ANE_DELTA: GameResource = GameResource::Image(2);
