//! `SharedTensor`: an f16 tensor on an IOSurface.

use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_core_ml::MLMultiArray;
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferLockFlags, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferMetalCompatibilityKey, kCVPixelFormatType_OneComponent16Half,
};
use objc2_foundation::{NSArray, NSNumber};
use objc2_io_surface::IOSurfaceRef;

use crate::error::{CoreMlError, Result};

struct Inner {
    shape: Vec<usize>,
    width: usize,
    height: usize,
    buffer: CFRetained<CVPixelBuffer>,
    surface: CFRetained<IOSurfaceRef>,
    array: Retained<MLMultiArray>,
}

// SAFETY: the pixel buffer, the IOSurface and the multi array are reference counted objects that `Inner` never mutates
// after `new`. Which side reads or writes the pages when is the business of the caller, who orders the Neural Engine
// and the GPU with a completion callback and an event.
unsafe impl Send for Inner {}
// SAFETY: see `Send`.
unsafe impl Sync for Inner {}

/// An f16 tensor of a given shape, in memory that Core ML, the Neural Engine and Metal share.
///
/// The memory is an IOSurface behind a `CVPixelBuffer` of format `OneComponent16Half`. The last dimension of the shape
/// is the width in pixels and the product of all other dimensions is the height, so the channels of a
/// `[1, C, H, W]` tensor lie one under the other as `C * H` rows of `W` values. A row may be padded: use
/// [`row_bytes`](Self::row_bytes) and never `width * 2` as the pitch. Clones share the memory.
#[derive(Clone)]
pub struct SharedTensor {
    inner: Arc<Inner>,
}

impl SharedTensor {
    /// Allocates a tensor of this shape. Every dimension must be at least 1 and the shape must have at least one
    /// dimension. The contents are unspecified until written.
    pub fn new(shape: &[usize]) -> Result<Self> {
        if shape.is_empty() || shape.contains(&0) {
            return Err(CoreMlError::Invalid(format!(
                "tensor shape {shape:?} must have dimensions of at least 1"
            )));
        }
        let width = shape[shape.len() - 1];
        let height = shape[..shape.len() - 1]
            .iter()
            .try_fold(1usize, |a, &d| a.checked_mul(d))
            .ok_or_else(|| CoreMlError::Invalid("tensor shape overflows".into()))?;
        let empty = CFDictionary::<CFString, CFType>::empty();
        // SAFETY: the extern statics are CoreVideo constants that live for the whole process.
        let (io_key, metal_key) = unsafe {
            (
                kCVPixelBufferIOSurfacePropertiesKey,
                kCVPixelBufferMetalCompatibilityKey,
            )
        };
        let yes = CFBoolean::new(true);
        let keys: [&CFString; 2] = [io_key, metal_key];
        let io_value: &CFType = empty.as_ref();
        let values: [&CFType; 2] = [io_value, yes.as_ref()];
        let attrs = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
        let mut out: Option<CFRetained<CVPixelBuffer>> = None;
        // SAFETY: the attributes are a valid dictionary and `out` is `None`, as the call requires.
        let r = unsafe {
            CVPixelBuffer::new(
                None,
                width,
                height,
                kCVPixelFormatType_OneComponent16Half,
                Some(&attrs),
                &mut out,
            )
        };
        let buffer = match (r, out) {
            (Ok(()), Some(b)) => b,
            (r, _) => {
                return Err(CoreMlError::Tensor(format!(
                    "CVPixelBufferCreate {width}x{height} failed: {r:?}"
                )));
            }
        };
        let surface = CVPixelBuffer::io_surface(Some(&buffer))
            .ok_or_else(|| CoreMlError::Tensor("the pixel buffer has no IOSurface".into()))?;
        let dims: Vec<Retained<NSNumber>> = shape
            .iter()
            .map(|&d| NSNumber::new_isize(d as isize))
            .collect();
        let refs: Vec<&NSNumber> = dims.iter().map(|d| &**d).collect();
        let ns_shape = NSArray::from_slice(&refs);
        // SAFETY: the last dimension is the width and the product of the rest is the height, as the initialiser needs.
        let array = unsafe {
            MLMultiArray::initWithPixelBuffer_shape(MLMultiArray::alloc(), &buffer, &ns_shape)
        };
        Ok(Self {
            inner: Arc::new(Inner {
                shape: shape.to_vec(),
                width,
                height,
                buffer,
                surface,
                array,
            }),
        })
    }

    /// The shape, outermost dimension first.
    pub fn shape(&self) -> &[usize] {
        &self.inner.shape
    }

    /// The width in pixels: the last dimension.
    pub fn width(&self) -> usize {
        self.inner.width
    }

    /// The height in pixels: the product of all other dimensions.
    pub fn height(&self) -> usize {
        self.inner.height
    }

    /// The number of f16 values.
    pub fn len(&self) -> usize {
        self.inner.width * self.inner.height
    }

    /// Always false: a tensor has at least one value.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The distance between two rows in bytes. It is at least `width * 2` and may be more.
    pub fn row_bytes(&self) -> usize {
        self.inner.buffer.bytes_per_row()
    }

    /// The IOSurface behind the tensor. `nxkit-metal` wraps it as a texture of `R16Float`, `width` by `height`.
    pub fn io_surface(&self) -> &IOSurfaceRef {
        &self.inner.surface
    }

    /// The multi array that Core ML sees.
    pub(crate) fn multi_array(&self) -> &Retained<MLMultiArray> {
        &self.inner.array
    }

    /// Writes the values, row-major without padding, as the bits of f16. `values.len()` must be [`len`](Self::len).
    /// Nobody else may use the tensor meanwhile.
    pub fn write_f16_bits(&self, values: &[u16]) -> Result<()> {
        if values.len() != self.len() {
            return Err(CoreMlError::Invalid(format!(
                "{} values for a tensor of {}",
                values.len(),
                self.len()
            )));
        }
        self.with_pages(false, |base, pitch| {
            for (y, row) in values.chunks_exact(self.inner.width).enumerate() {
                // SAFETY: the lock holds `height` rows of `pitch` bytes at `base`, and a row has `width` values.
                unsafe {
                    ptr::copy_nonoverlapping(
                        row.as_ptr(),
                        base.add(y * pitch).cast::<u16>(),
                        row.len(),
                    );
                }
            }
        })
    }

    /// Locks the pages and calls `f` with all of them: `height` rows of [`row_bytes`](Self::row_bytes) bytes, the second
    /// argument being that pitch. For a writer that fills rows itself, in several threads. Nobody else may use the
    /// tensor meanwhile.
    pub fn write_bytes(&self, f: impl FnOnce(&mut [u8], usize)) -> Result<()> {
        self.with_pages(false, |base, pitch| {
            // SAFETY: the lock holds `height` rows of `pitch` bytes at `base`, valid for writes until it is released
            // after `f`, and the caller is the only user of the tensor.
            let all = unsafe { std::slice::from_raw_parts_mut(base, pitch * self.inner.height) };
            f(all, pitch);
        })
    }

    /// Reads the values, row-major without padding, as the bits of f16. `out.len()` must be [`len`](Self::len).
    /// Nobody else may write the tensor meanwhile.
    pub fn read_f16_bits(&self, out: &mut [u16]) -> Result<()> {
        if out.len() != self.len() {
            return Err(CoreMlError::Invalid(format!(
                "{} values for a tensor of {}",
                out.len(),
                self.len()
            )));
        }
        self.with_pages(true, |base, pitch| {
            for (y, row) in out.chunks_exact_mut(self.inner.width).enumerate() {
                // SAFETY: as in `write_f16_bits`.
                unsafe {
                    ptr::copy_nonoverlapping(
                        base.add(y * pitch).cast::<u16>(),
                        row.as_mut_ptr(),
                        row.len(),
                    );
                }
            }
        })
    }

    fn with_pages(&self, read_only: bool, f: impl FnOnce(*mut u8, usize)) -> Result<()> {
        let flags = if read_only {
            CVPixelBufferLockFlags::ReadOnly
        } else {
            CVPixelBufferLockFlags(0)
        };
        let b = &*self.inner.buffer;
        // SAFETY: the buffer is alive, and the lock is released below on every path.
        if let Err(e) = unsafe { b.lock_base_address(flags) } {
            return Err(CoreMlError::Tensor(format!("lock failed: {e}")));
        }
        let base: *mut c_void = b.base_address();
        f(base.cast::<u8>(), b.bytes_per_row());
        // SAFETY: locked above with the same flags.
        let _ = unsafe { b.unlock_base_address(flags) };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_shapes() {
        assert!(SharedTensor::new(&[]).is_err());
        assert!(SharedTensor::new(&[1, 0, 4]).is_err());
    }

    #[test]
    fn planar_shape_is_channels_as_rows() {
        let Ok(t) = SharedTensor::new(&[1, 3, 8, 16]) else {
            panic!("allocation failed")
        };
        assert_eq!((t.width(), t.height(), t.len()), (16, 24, 384));
        assert!(t.row_bytes() >= 32);
    }

    #[test]
    fn write_bytes_gives_every_row_at_the_pitch() {
        let Ok(t) = SharedTensor::new(&[3, 7]) else {
            panic!("allocation failed")
        };
        let rows = t.height();
        let r = t.write_bytes(|all, pitch| {
            assert_eq!(all.len(), pitch * rows);
            for (y, row) in all.chunks_exact_mut(pitch).enumerate() {
                for v in row[..14].as_chunks_mut::<2>().0 {
                    *v = (y as u16 + 1).to_le_bytes();
                }
            }
        });
        assert!(r.is_ok());
        let mut out = vec![0u16; t.len()];
        assert!(t.read_f16_bits(&mut out).is_ok());
        assert!(
            out.as_chunks::<7>()
                .0
                .iter()
                .enumerate()
                .all(|(y, r)| r.iter().all(|&v| v == y as u16 + 1))
        );
    }

    #[test]
    fn values_round_trip_through_the_padded_rows() {
        // A width that is unlikely to be a multiple of the row alignment.
        let Ok(t) = SharedTensor::new(&[2, 5, 7]) else {
            panic!("allocation failed")
        };
        let src: Vec<u16> = (0..t.len() as u16).map(|i| i.wrapping_mul(31)).collect();
        assert!(t.write_f16_bits(&src).is_ok());
        let mut out = vec![0u16; t.len()];
        assert!(t.read_f16_bits(&mut out).is_ok());
        assert_eq!(src, out);
        assert!(t.write_f16_bits(&src[1..]).is_err());
    }
}
