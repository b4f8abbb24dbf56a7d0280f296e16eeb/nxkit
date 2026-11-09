//! The input memory that the game fills.

/// The input memory that the game fills. The game writes to it, so it is raw memory and not a Rust slice.
pub(super) struct Memory {
    pub(super) ptr: *mut u8,
    len: usize,
}

// SAFETY: the memory is plain bytes that are allocated once and freed in `Drop`. All reads go through raw pointer
// copies.
unsafe impl Send for Memory {}
// SAFETY: as for `Send`.
unsafe impl Sync for Memory {}

impl Memory {
    pub(super) fn new(len: usize) -> Memory {
        let slice: Box<[u8]> = vec![0u8; len].into_boxed_slice();
        Memory {
            ptr: Box::into_raw(slice).cast::<u8>(),
            len,
        }
    }

    pub(super) fn read(&self, offset: usize, size: usize) -> Vec<u8> {
        let mut v = vec![0u8; size];
        if offset + size <= self.len {
            // SAFETY: the range is inside the allocation and `v` has `size` bytes. A write of the game at the same time
            // gives torn bytes, not undefined behaviour of the allocation.
            unsafe { std::ptr::copy_nonoverlapping(self.ptr.add(offset), v.as_mut_ptr(), size) };
        }
        v
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        // SAFETY: `ptr` and `len` come from `Box::into_raw` of a boxed slice in `new` and are freed only here.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(self.ptr, self.len)) });
    }
}
