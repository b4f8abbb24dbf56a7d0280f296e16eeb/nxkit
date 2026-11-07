//! Compute indirect command buffers and the layout of an indirect execution range.
//!
//! A compute ICB holds dispatches. A compute pass runs a range of it with `ComputeCommand::ExecuteIcb`. The facts
//! come from `MTLIndirectCommandBuffer.rs` and `MTLIndirectCommandEncoder.rs` of the pinned `objc2-metal`:
//! - A command sets the pipeline, kernel buffers (an `MTLBuffer` plus an offset), a dispatch by threadgroups or by
//!   threads, and a barrier.
//! - A compute ICB cannot hold draw commands. The command types are fixed when it is made.
//! - A pipeline used in it needs `supportIndirectCommandBuffers` (`PipelineCache::compute_icb`).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSRange;
use objc2_metal::{
    MTLAllocation, MTLDevice, MTLIndirectCommandBuffer, MTLIndirectCommandBufferDescriptor,
    MTLIndirectCommandBufferExecutionRange, MTLIndirectCommandType, MTLIndirectComputeCommand,
    MTLResourceOptions, MTLSize,
};

use crate::databuf::DataBuffer;
use crate::device::Context;
use crate::error::{MetalError, Result, create};
use crate::icb::IcbBuffer;
use crate::pipeline::ComputePipeline;
use crate::residency::ResidentGuard;
use crate::table::Keep;

/// The range of commands that an indirect execution reads from a GPU buffer. It is 8 bytes: `location`, then
/// `length`, both as little-endian u32.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExecutionRange {
    /// The first command.
    pub location: u32,
    /// The number of commands.
    pub length: u32,
}

impl ExecutionRange {
    /// The size of the range in a buffer.
    pub const BYTES: usize = std::mem::size_of::<MTLIndirectCommandBufferExecutionRange>();

    /// The bytes to write into the GPU buffer.
    pub fn to_bytes(self) -> [u8; 8] {
        let mut b = [0u8; 8];
        b[..4].copy_from_slice(&self.location.to_le_bytes());
        b[4..].copy_from_slice(&self.length.to_le_bytes());
        b
    }
}

/// Checks that an execution range fits in its buffer.
pub(crate) fn check_range_buffer(buffer: &DataBuffer, offset: u64) -> Result<()> {
    if offset % 4 != 0 || offset + ExecutionRange::BYTES as u64 > buffer.len() {
        return Err(MetalError::Invalid(
            "the execution range is misaligned or outside its buffer",
        ));
    }
    Ok(())
}

/// What a compute ICB is made for.
#[derive(Clone, Copy, Debug)]
pub struct ComputeIcbDesc {
    /// The most commands it can hold.
    pub max_commands: u32,
    /// The kernel buffer slots a command can bind.
    pub max_kernel_buffers: u32,
}

/// One command of a compute ICB.
#[derive(Clone)]
pub enum ComputeIcbCommand {
    /// Sets the pipeline. It must have been made with `PipelineCache::compute_icb`.
    SetPipeline(ComputePipeline),
    /// Binds guest memory to a kernel buffer slot.
    SetKernelBuffer {
        /// The slot.
        index: u32,
        /// The buffer.
        buffer: IcbBuffer,
    },
    /// Binds a data buffer to a kernel buffer slot.
    SetKernelData {
        /// The slot.
        index: u32,
        /// The buffer.
        buffer: Arc<DataBuffer>,
        /// The byte offset in the buffer.
        offset: u64,
    },
    /// Runs whole threadgroups.
    DispatchThreadgroups {
        /// The threadgroups per grid.
        groups: [u32; 3],
        /// The threads per threadgroup.
        threads: [u32; 3],
    },
    /// Runs `grid` threads, with partial threadgroups at the edge.
    DispatchThreads {
        /// The threads per grid.
        grid: [u32; 3],
        /// The threads per threadgroup.
        threads: [u32; 3],
    },
    /// Makes this command wait for the commands before it.
    Barrier,
}

/// An indirect command buffer of dispatches.
#[derive(Clone)]
pub struct ComputeIndirectCommandBuffer {
    pub(crate) raw: Retained<ProtocolObject<dyn MTLIndirectCommandBuffer>>,
    size: u32,
    max_kernel: u32,
    keep: Arc<Mutex<Vec<Keep>>>,
    _resident: Arc<ResidentGuard>,
}

// SAFETY: an ICB is a thread safe Metal object. The caller encodes one slot on one thread at a time and not while
// the GPU runs the ICB.
unsafe impl Send for ComputeIndirectCommandBuffer {}
// SAFETY: see `Send`.
unsafe impl Sync for ComputeIndirectCommandBuffer {}

fn size3(v: [u32; 3]) -> MTLSize {
    MTLSize {
        width: v[0] as usize,
        height: v[1] as usize,
        depth: v[2] as usize,
    }
}

impl Context {
    /// Makes an ICB for dispatches. It joins the residency set.
    pub fn new_compute_indirect_command_buffer(
        &self,
        desc: &ComputeIcbDesc,
    ) -> Result<ComputeIndirectCommandBuffer> {
        if desc.max_commands == 0 {
            return Err(MetalError::Invalid("an ICB needs at least one command"));
        }
        let d = MTLIndirectCommandBufferDescriptor::new();
        d.setCommandTypes(
            MTLIndirectCommandType::ConcurrentDispatch
                | MTLIndirectCommandType::ConcurrentDispatchThreads,
        );
        d.setInheritPipelineState(false);
        d.setInheritBuffers(false);
        d.setMaxKernelBufferBindCount(desc.max_kernel_buffers as usize);
        // SAFETY: `max_commands` is the size the ICB is made for and every later index is checked against it.
        let raw = unsafe {
            self.device()
                .newIndirectCommandBufferWithDescriptor_maxCommandCount_options(
                    &d,
                    desc.max_commands as usize,
                    MTLResourceOptions::StorageModePrivate,
                )
        }
        .ok_or_else(|| create("compute indirect command buffer", "Metal returned none"))?;
        let resident = self.make_resident(ProtocolObject::<dyn MTLAllocation>::from_ref(&*raw));
        Ok(ComputeIndirectCommandBuffer {
            raw,
            size: desc.max_commands,
            max_kernel: desc.max_kernel_buffers,
            keep: Arc::new(Mutex::new(Vec::new())),
            _resident: Arc::new(resident),
        })
    }
}

impl ComputeIndirectCommandBuffer {
    /// The number of commands it can hold.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Empties the ICB and drops what it kept alive.
    pub fn reset(&self) {
        // SAFETY: the range is the whole buffer.
        unsafe {
            self.raw.resetWithRange(NSRange::new(0, self.size as usize));
        }
        self.keep().clear();
    }

    /// Encodes the commands of one slot. The slot is overwritten. The GPU must not execute the ICB while this runs.
    pub fn encode(&self, slot: u32, commands: &[ComputeIcbCommand]) -> Result<()> {
        if slot >= self.size {
            return Err(MetalError::Invalid("the ICB slot is out of range"));
        }
        // SAFETY: `slot` is below the size (checked above).
        let cmd = unsafe { self.raw.indirectComputeCommandAtIndex(slot as usize) };
        for c in commands {
            self.encode_one(&cmd, c)?;
        }
        Ok(())
    }

    fn encode_one(
        &self,
        cmd: &ProtocolObject<dyn MTLIndirectComputeCommand>,
        c: &ComputeIcbCommand,
    ) -> Result<()> {
        match c {
            ComputeIcbCommand::SetPipeline(p) => {
                cmd.setComputePipelineState(&p.raw);
                self.keep().push(Keep::new(p.clone()));
            }
            ComputeIcbCommand::SetKernelBuffer { index, buffer } => {
                if *index >= self.max_kernel {
                    return Err(MetalError::Invalid("the kernel buffer slot is out of range"));
                }
                if buffer.offset >= buffer.guest.len() {
                    return Err(MetalError::Invalid(
                        "the buffer offset is outside the guest buffer",
                    ));
                }
                // SAFETY: the slot is below the bind count and the offset is inside the guest buffer (checked
                // above). The ICB keeps the guest buffer alive and it is resident.
                unsafe {
                    cmd.setKernelBuffer_offset_atIndex(
                        buffer.guest.buffer(),
                        buffer.offset as usize,
                        *index as usize,
                    )
                };
                self.keep().push(Keep::new(buffer.guest.clone()));
            }
            ComputeIcbCommand::SetKernelData {
                index,
                buffer,
                offset,
            } => {
                if *index >= self.max_kernel || *offset >= buffer.len() {
                    return Err(MetalError::Invalid(
                        "the kernel data slot or offset is out of range",
                    ));
                }
                // SAFETY: the slot is below the bind count and the offset is inside the buffer (checked above).
                unsafe {
                    cmd.setKernelBuffer_offset_atIndex(
                        &buffer.raw,
                        *offset as usize,
                        *index as usize,
                    )
                };
                self.keep().push(Keep::new(buffer.clone()));
            }
            ComputeIcbCommand::DispatchThreadgroups { groups, threads } => cmd
                .concurrentDispatchThreadgroups_threadsPerThreadgroup(
                    size3(*groups),
                    size3(*threads),
                ),
            ComputeIcbCommand::DispatchThreads { grid, threads } => cmd
                .concurrentDispatchThreads_threadsPerThreadgroup(size3(*grid), size3(*threads)),
            ComputeIcbCommand::Barrier => cmd.setBarrier(),
        }
        Ok(())
    }

    fn keep(&self) -> MutexGuard<'_, Vec<Keep>> {
        self.keep.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
