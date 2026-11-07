//! What the front end needs from the GPU backend.
//!
//! `nxkit-gpu` implements [`Backend`]. The FFI layer in `nxkit-rt` calls it through one `Arc<dyn Backend>`.
//! The trait uses only types of this crate, so the front end does not know Metal.
//!
//! The backend sees the work of the game at these points. The optional notices ([`Backend::device_modes`],
//! [`Backend::acquire`], [`Backend::submit`], [`Backend::program_set`]) have empty defaults.
//!
//! - A memory pool appears or goes away ([`Backend::register_pool`], [`Backend::unregister_pool`]). The backend maps the
//!   range as a Metal buffer without copies. The GPU address of the pool is then `gpu_base + (host - host_base)`.
//! - A frame ends ([`Backend::flush_frame`]). The call hands over the commands, the pass plan and a [`Fence`].
//! - The game presents a window texture ([`Backend::present`]). It follows the flush of the frame that drew it.
//! - The CPU waits ([`Backend::wait_fence`], [`Backend::wait_idle`]).
//! - A texture or a sampler is created or released ([`Backend::texture_created`] and its relatives), so the backend
//!   can keep its own objects for them.
//!
//! Fences count flushes. The front end numbers the flushes 1, 2, 3 and so on and passes the number to
//! [`Backend::flush_frame`]. The backend marks the fence done when the GPU has finished that flush. A fence is done when
//! it and every earlier fence are done. Fence 0 is always done.

use std::fmt;

use crate::cmdbuf::CommandHandle;
use crate::frame::Frame;
use crate::front::FlushReason;
use crate::passes::PassPlan;
use crate::resource::{TexId, TextureRecord};
use crate::tic::Tsc;

/// A failure that the backend reports. The text is for the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendError(pub String);

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GPU backend: {}", self.0)
    }
}

impl std::error::Error for BackendError {}

/// The result type of the backend.
pub type BackendResult<T> = std::result::Result<T, BackendError>;

/// The number of a flush. The GPU finishes the fences in order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fence(pub u64);

/// A memory pool that the game made (`nvnMemoryPoolInitialize`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolInfo {
    /// The host address of the first byte. It is also the guest address.
    pub host: u64,
    /// The size in bytes.
    pub len: u64,
    /// The `NVNmemoryPoolFlags` of the game.
    pub flags: u32,
}

/// The crop rectangle of a window (`NVNrectangle`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    /// The left edge.
    pub x: i32,
    /// The top edge.
    pub y: i32,
    /// The width. Zero with zero height means no crop.
    pub width: i32,
    /// The height.
    pub height: i32,
}

/// One present of a window texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Present {
    /// The guest address of the `NVNwindow`. It names the window for the backend.
    pub window: u64,
    /// The texture that the game presents.
    pub texture: TexId,
    /// The present interval. Zero presents at once, n waits for n vertical blanks.
    pub interval: i32,
    /// The crop of the window.
    pub crop: Rect,
}

/// The key of one shader stage: the first 8 bytes of `sha256(control section key bytes || code)`.
/// It is the key of the shader in the `_b.json` of the shader step, as 16 lower-case hex digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShaderKey(pub [u8; 8]);

impl ShaderKey {
    /// The 16 hex digits that name the shader in `_b.json`.
    pub fn to_hex(self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// One stage of a program (`NVNshaderData`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageKey {
    /// The position of the stage in the array that the game gave `nvnProgramSetShaders`.
    pub index: u8,
    /// The key of the stage. None when the code area did not lie in guest memory at the call, as when a game sets a
    /// program before it fills the memory. The host computes the key at the first use of the program.
    pub key: Option<ShaderKey>,
    /// The host address of the code area (`NVNshaderData::data`).
    pub code: u64,
    /// The length of the code area in bytes.
    pub code_len: u32,
}

/// The result of a wait on a fence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitResult {
    /// The fence was done when the wait started.
    AlreadyDone,
    /// The fence became done during the wait.
    Done,
    /// The time ran out.
    TimedOut,
    /// The wait failed.
    Failed,
}

/// The GPU backend as the front end sees it. A call may come from any guest thread.
pub trait Backend: Send + Sync {
    /// The GPU address that Metal gave to the first byte of the mapped guest memory. The front end builds its
    /// `LinearSpace` from this value and the range of guest memory.
    fn gpu_base(&self) -> u64;

    /// A memory pool appears. The range lies in the mapped guest memory.
    fn register_pool(&self, pool: PoolInfo) -> BackendResult<()>;

    /// A memory pool goes away. `host` is the address that [`PoolInfo`] gave.
    fn unregister_pool(&self, host: u64);

    /// A frame ends. `fence` is the number of this flush. The backend encodes `frame` as `plan` says and marks
    /// `fence` done when the GPU has finished.
    fn flush_frame(
        &self,
        frame: &Frame,
        plan: &PassPlan,
        reason: FlushReason,
        fence: Fence,
    ) -> BackendResult<()>;

    /// The game presents a window texture. The frame that drew it was flushed before this call.
    fn present(&self, present: Present) -> BackendResult<()>;

    /// Waits until `fence` is done or `timeout_ns` nanoseconds pass. `u64::MAX` waits without a limit.
    fn wait_fence(&self, fence: Fence, timeout_ns: u64) -> WaitResult;

    /// Waits until the GPU has finished everything that was flushed (`nvnQueueFinish`).
    fn wait_idle(&self);

    /// The game gave a program its stages (`nvnProgramSetShaders`). `program` is the guest address of the `NVNprogram`.
    /// The backend may start to load the Metal functions of the keys. A later call for the same program replaces the
    /// earlier one. The default does nothing.
    fn program_set(&self, _program: u64, _stages: &[StageKey]) {}

    /// A program was finalised (`nvnProgramFinalize`). The default does nothing.
    fn program_released(&self, _program: u64) {}

    /// The game set the window origin or the depth mode of its device. The default does nothing.
    fn device_modes(&self, _origin_lower_left: bool, _depth_zero_to_one: bool) {}

    /// The game acquires the texture `index` of a window. The call may block until the display can take another frame
    /// (vertical sync pacing). The default does not block.
    fn acquire(&self, _window: u64, _index: u32) {}

    /// The game submitted `handles` on `queue`, after the front end accepted them. `queue` is the guest address of the
    /// `NVNqueue`. `origin_lower_left` is the window origin that the queue captured at its creation. The frame holds
    /// the analysis of the commands, so the backend replays the recordings through these handles. The default does
    /// nothing.
    fn submit(&self, _queue: u64, _handles: &[CommandHandle], _origin_lower_left: bool) {}

    /// A texture was created in guest memory (`nvnTextureInitialize`).
    fn texture_created(&self, record: &TextureRecord);

    /// A texture was finalised (`nvnTextureFinalize`).
    fn texture_finalized(&self, id: TexId);

    /// The game registered a sampler in slot `id` of a sampler pool.
    fn sampler_registered(&self, id: u32, sampler: &Tsc);

    /// The game dropped the sampler pool that held slot `id`.
    fn sampler_released(&self, id: u32);
}

/// A backend that records its calls and does nothing else. Tests use it.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod null {
    use super::*;
    use std::sync::Mutex;

    /// One call of the front end.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) enum Call {
        RegisterPool(PoolInfo),
        UnregisterPool(u64),
        Flush {
            commands: usize,
            reason: FlushReason,
            fence: Fence,
        },
        Present(Present),
        WaitFence(Fence, u64),
        WaitIdle,
        TextureCreated(TexId),
        TextureFinalized(TexId),
        SamplerRegistered(u32),
        SamplerReleased(u32),
    }

    /// The no-op backend.
    #[derive(Debug, Default)]
    pub(crate) struct NullBackend {
        pub(crate) calls: Mutex<Vec<Call>>,
    }

    impl NullBackend {
        fn log(&self, c: Call) {
            if let Ok(mut v) = self.calls.lock() {
                v.push(c);
            }
        }

        pub(crate) fn calls(&self) -> Vec<Call> {
            self.calls.lock().map(|v| v.clone()).unwrap_or_default()
        }
    }

    impl Backend for NullBackend {
        fn gpu_base(&self) -> u64 {
            0x15_0400_8000
        }

        fn register_pool(&self, pool: PoolInfo) -> BackendResult<()> {
            self.log(Call::RegisterPool(pool));
            Ok(())
        }

        fn unregister_pool(&self, host: u64) {
            self.log(Call::UnregisterPool(host));
        }

        fn flush_frame(
            &self,
            frame: &Frame,
            _plan: &PassPlan,
            reason: FlushReason,
            fence: Fence,
        ) -> BackendResult<()> {
            self.log(Call::Flush {
                commands: frame.cmds.len(),
                reason,
                fence,
            });
            Ok(())
        }

        fn present(&self, present: Present) -> BackendResult<()> {
            self.log(Call::Present(present));
            Ok(())
        }

        fn wait_fence(&self, fence: Fence, timeout_ns: u64) -> WaitResult {
            self.log(Call::WaitFence(fence, timeout_ns));
            WaitResult::Done
        }

        fn wait_idle(&self) {
            self.log(Call::WaitIdle);
        }

        fn texture_created(&self, record: &TextureRecord) {
            self.log(Call::TextureCreated(record.id));
        }

        fn texture_finalized(&self, id: TexId) {
            self.log(Call::TextureFinalized(id));
        }

        fn sampler_registered(&self, id: u32, _sampler: &Tsc) {
            self.log(Call::SamplerRegistered(id));
        }

        fn sampler_released(&self, id: u32) {
            self.log(Call::SamplerReleased(id));
        }
    }

    #[test]
    fn the_null_backend_records_what_it_is_told() {
        let b: Box<dyn Backend> = Box::new(NullBackend::default());
        b.wait_idle();
        assert_eq!(b.wait_fence(Fence(2), 5), WaitResult::Done);
        let pool = PoolInfo {
            host: 1,
            len: 2,
            flags: 0,
        };
        assert!(b.register_pool(pool).is_ok());
    }
}
