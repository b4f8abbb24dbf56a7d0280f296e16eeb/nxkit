//! Memory that the host owns and keeps alive while the mixer reads it.

#![allow(unsafe_code)]

/// Gives `f` the `len` bytes at `addr`. The memory must stay alive and unchanged while `f` runs. Two kinds of memory
/// qualify. One is a block that the host allocated and keeps until the mixer returned the buffer as released. The other
/// is the SDK's command list and mix buffers, which the renderer reads while it holds the render lock.
///
/// The OutputStream take-over makes host blocks, because it converts the game's frames to 16-bit before it queues them.
pub fn with_host<R>(addr: u64, len: u64, f: impl FnOnce(&[u8]) -> R) -> R {
    if addr == 0 || len == 0 {
        return f(&[]);
    }
    // SAFETY: `addr` and `len` describe memory that stays alive and unchanged during the call, as the doc states: a block
    // that the host frees only after the mixer gave it back, or SDK memory that the caller keeps stable with the render
    // lock. The callback does not use the slice after it returns.
    let s = unsafe { std::slice::from_raw_parts(addr as *const u8, len as usize) };
    f(s)
}
