//! The queue group: `nvnQueue*` and the queue builder.
//!
//! The `NVNqueue` is 0x2000 bytes in guest memory. The host uses the first 0x20 bytes as plain data and writes no more:
//!
//! | offset | field |
//! | --- | --- |
//! | 0 | magic `QUEU` while initialised |
//! | 8 | the guest address of the `NVNdevice` |
//! | 16 | the number of submits (u64) |
//! | 24 | the window origin captured at the initialisation, 1 for lower left (u32) |
//!
//! The origin is captured once, as NVN writes it into the initial GPU state of the queue. A later change of the
//! device origin does not change the queue.
//!
//! The queue builder is 0x40 bytes in the headers and the host writes all of them: device at 0, flags at 8, command, compute and control memory sizes at 16, 24 and 32,
//! flush threshold at 40, queue memory at 48 and its size at 56. The host only stores them. It does not use the queue
//! memory, because it executes the recordings itself.
//!
//! The state that a submit leaves is the state that the next submit on the same queue starts with. The front end
//! keeps it by the guest address of the queue (`FrontEnd::submit_on`). `nvnQueueFinalize` drops it.

use nxkit_nvn::backend::Present;
use nxkit_nvn::cmdbuf::CommandHandle;

use super::abi::{AbiResult, Obj, guard, log_once, magic, other, table, used};
use super::ctx::gpu;
use super::device;
use super::sync;
use super::window;

/// The magic of an initialised queue.
const QUEU: u32 = magic(b'Q', b'U', b'E', b'U');

const OFF_DEV: u64 = 8;
const OFF_SUBMITS: u64 = 16;
const OFF_ORIGIN_LL: u64 = 24;

const B_DEV: u64 = 0;
const B_FLAGS: u64 = 8;
const B_CMD: u64 = 16;
const B_COMP: u64 = 24;
const B_CTL: u64 = 32;
const B_FLUSH: u64 = 40;
const B_MEM: u64 = 48;
const B_MEM_SIZE: u64 = 56;

/// The sizes of a queue builder that `nvnQueueBuilderSetDefaults` sets (the defaults of the device).
const DEFAULT_COMMAND: u64 = 65536;
const DEFAULT_COMPUTE: u64 = 262144;
const DEFAULT_CONTROL: u64 = 16384;
const DEFAULT_FLUSH: u64 = 4096;
/// `NVN_QUEUE_GET_ERROR_RESULT_GPU_NO_ERROR` and `NVN_QUEUE_ACQUIRE_TEXTURE_RESULT_SUCCESS`.
const SUCCESS: u32 = 0;

fn builder_set_u64(name: &'static str, b: u64, off: u64, v: u64) {
    guard(name, || Obj(b).set_u64(off, v));
}

extern "C" fn builder_set_defaults(b: u64) {
    guard("nvnQueueBuilderSetDefaults", || {
        let o = Obj(b);
        let dev = o.u64(B_DEV)?;
        o.zero(used::QUEUE_BUILDER)?;
        o.set_u64(B_DEV, dev)?;
        o.set_u64(B_CMD, DEFAULT_COMMAND)?;
        o.set_u64(B_COMP, DEFAULT_COMPUTE)?;
        o.set_u64(B_CTL, DEFAULT_CONTROL)?;
        o.set_u64(B_FLUSH, DEFAULT_FLUSH)
    });
}

extern "C" fn builder_set_device(b: u64, dev: u64) {
    builder_set_u64("nvnQueueBuilderSetDevice", b, B_DEV, dev);
}

extern "C" fn builder_set_flags(b: u64, flags: i32) {
    guard("nvnQueueBuilderSetFlags", || Obj(b).set_i32(B_FLAGS, flags));
}

extern "C" fn builder_set_command_memory_size(b: u64, n: u64) {
    builder_set_u64("nvnQueueBuilderSetCommandMemorySize", b, B_CMD, n);
}

extern "C" fn builder_set_compute_memory_size(b: u64, n: u64) {
    builder_set_u64("nvnQueueBuilderSetComputeMemorySize", b, B_COMP, n);
}

extern "C" fn builder_set_control_memory_size(b: u64, n: u64) {
    builder_set_u64("nvnQueueBuilderSetControlMemorySize", b, B_CTL, n);
}

extern "C" fn builder_set_command_flush_threshold(b: u64, n: u64) {
    builder_set_u64("nvnQueueBuilderSetCommandFlushThreshold", b, B_FLUSH, n);
}

extern "C" fn builder_set_queue_memory(b: u64, mem: u64, n: u64) {
    guard("nvnQueueBuilderSetQueueMemory", || {
        Obj(b).set_u64(B_MEM, mem)?;
        Obj(b).set_u64(B_MEM_SIZE, n)
    });
}

extern "C" fn builder_set_queue_priority(_b: u64, _priority: i32) {}

extern "C" fn builder_set_primary_queue(_b: u64, _primary: u64) {}

/// The queue memory size that the builder asks for: command, compute and control memory, rounded up to 4096.
fn queue_memory_size(command: u64, compute: u64, control: u64) -> u64 {
    command
        .wrapping_add(compute)
        .wrapping_add(control)
        .wrapping_add(4095)
        & !4095
}

extern "C" fn builder_get_queue_memory_size(b: u64) -> u64 {
    guard("nvnQueueBuilderGetQueueMemorySize", || {
        let o = Obj(b);
        Ok(queue_memory_size(
            o.u64(B_CMD)?,
            o.u64(B_COMP)?,
            o.u64(B_CTL)?,
        ))
    })
}

extern "C" fn queue_initialize(q: u64, b: u64) -> u32 {
    guard("nvnQueueInitialize", || {
        let dev = Obj(b).u64(B_DEV)?;
        let lower_left = device::origin_lower_left(dev)?;
        let o = Obj(q);
        o.zero(used::QUEUE)?;
        o.set_u32(0, QUEU)?;
        o.set_u64(OFF_DEV, dev)?;
        o.set_u32(OFF_ORIGIN_LL, u32::from(lower_left))?;
        Ok(1)
    })
}

extern "C" fn queue_finalize(q: u64) {
    guard("nvnQueueFinalize", || {
        Obj(q).set_u32(0, 0)?;
        gpu("nvnQueueFinalize").lock().drop_queue(q as usize);
        Ok(())
    });
}

extern "C" fn queue_set_debug_label(_q: u64, _label: u64) {}

/// `nvnQueueGetError`: the host has no GPU error to report, so the result is "no error" and the info stays untouched.
extern "C" fn queue_get_error(_q: u64, _info: u64) -> u32 {
    SUCCESS
}

extern "C" fn queue_submit_commands(q: u64, n: i32, handles: u64) {
    guard("nvnQueueSubmitCommands", || {
        let n =
            usize::try_from(n).map_err(|_| other(format!("the command count {n} is negative")))?;
        let list = Obj(handles);
        let handles = (0..n as u64)
            .map(|i| list.u64(8 * i).map(CommandHandle))
            .collect::<AbiResult<Vec<_>>>()?;
        let o = Obj(q);
        let origin = o.u32(OFF_ORIGIN_LL)? != 0;
        crate::diag!("DIAG submit q {q:#x} handles {handles:x?}");
        o.set_u64(OFF_SUBMITS, o.u64(OFF_SUBMITS)?.wrapping_add(1))?;
        gpu("nvnQueueSubmitCommands").submit(q, &handles, origin)
    });
}

/// `nvnQueueFlush`: the host replays a frame when the game presents or waits, so a flush has nothing to send.
extern "C" fn queue_flush(_q: u64) {}

extern "C" fn queue_finish(_q: u64) {
    guard("nvnQueueFinish", || gpu("nvnQueueFinish").finish());
}

extern "C" fn queue_present_texture(_q: u64, w: u64, index: i32) {
    guard("nvnQueuePresentTexture", || {
        let Some(view) = window::present_view(w, index)? else {
            log_once(
                "nvnQueuePresentTexture-index",
                format_args!(
                    "nvnQueuePresentTexture: the index {index} is not a texture of the window, nothing is presented"
                ),
            );
            return Ok(());
        };
        let g = gpu("nvnQueuePresentTexture");
        let texture = g.textures.get(view.texture).ok_or_else(|| {
            other(format!(
                "the window texture {index} at {:#x} is not an initialised texture",
                view.texture
            ))
        })?;
        let crop = crate::gfx::vi::present_crop(view.crop, g.texture_extent(view.texture));
        crate::gfx::vi::frame_queued();
        g.present(Present {
            window: w,
            texture,
            interval: view.interval,
            crop,
        })
    });
}

/// `nvnQueueAcquireTexture` (NVN 53): the same as the window acquire without a sync object.
extern "C" fn queue_acquire_texture(_q: u64, w: u64, idx: u64) -> u32 {
    guard("nvnQueueAcquireTexture", || {
        window::acquire("nvnQueueAcquireTexture", w, 0, idx)?;
        Ok(SUCCESS)
    })
}

extern "C" fn queue_fence_sync(_q: u64, s: u64, _condition: i32, _flags: i32) {
    guard("nvnQueueFenceSync", || {
        let fence = gpu("nvnQueueFenceSync").fence_now();
        sync::set_queue_fence(Obj(s), fence)
    });
}

/// `nvnQueueWaitSync`: the host replays in submission order, so the GPU never waits for an earlier fence.
extern "C" fn queue_wait_sync(_q: u64, s: u64) -> u32 {
    guard("nvnQueueWaitSync", || {
        if !sync::is_fenced(Obj(s))? {
            log_once(
                "nvnQueueWaitSync",
                "QueueWaitSync: the sync object has no fence yet, the host does not wait",
            );
        }
        Ok(1)
    })
}

/// The functions of the group.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnQueueBuilderSetDevice" => builder_set_device as extern "C" fn(u64, u64),
        "nvnQueueBuilderSetDefaults" => builder_set_defaults as extern "C" fn(u64),
        "nvnQueueBuilderSetFlags" => builder_set_flags as extern "C" fn(u64, i32),
        "nvnQueueBuilderSetCommandMemorySize" => builder_set_command_memory_size as extern "C" fn(u64, u64),
        "nvnQueueBuilderSetComputeMemorySize" => builder_set_compute_memory_size as extern "C" fn(u64, u64),
        "nvnQueueBuilderSetControlMemorySize" => builder_set_control_memory_size as extern "C" fn(u64, u64),
        "nvnQueueBuilderSetCommandFlushThreshold" => builder_set_command_flush_threshold as extern "C" fn(u64, u64),
        "nvnQueueBuilderSetQueueMemory" => builder_set_queue_memory as extern "C" fn(u64, u64, u64),
        "nvnQueueBuilderSetQueuePriority" => builder_set_queue_priority as extern "C" fn(u64, i32),
        "nvnQueueBuilderSetPrimaryQueue" => builder_set_primary_queue as extern "C" fn(u64, u64),
        "nvnQueueBuilderGetQueueMemorySize" => builder_get_queue_memory_size as extern "C" fn(u64) -> u64,
        "nvnQueueInitialize" => queue_initialize as extern "C" fn(u64, u64) -> u32,
        "nvnQueueFinalize" => queue_finalize as extern "C" fn(u64),
        "nvnQueueSetDebugLabel" => queue_set_debug_label as extern "C" fn(u64, u64),
        "nvnQueueSubmitCommands" => queue_submit_commands as extern "C" fn(u64, i32, u64),
        "nvnQueueFlush" => queue_flush as extern "C" fn(u64),
        "nvnQueueFinish" => queue_finish as extern "C" fn(u64),
        "nvnQueuePresentTexture" => queue_present_texture as extern "C" fn(u64, u64, i32),
        "nvnQueueAcquireTexture" => queue_acquire_texture as extern "C" fn(u64, u64, u64) -> u32,
        "nvnQueueFenceSync" => queue_fence_sync as extern "C" fn(u64, u64, i32, i32),
        "nvnQueueWaitSync" => queue_wait_sync as extern "C" fn(u64, u64) -> u32,
        "nvnQueueGetError" => queue_get_error as extern "C" fn(u64, u64) -> u32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvn::abi::testing;

    #[test]
    fn the_queue_memory_size_rounds_up_to_a_page() {
        assert_eq!(queue_memory_size(65536, 262144, 16384), 344064);
        assert_eq!(queue_memory_size(1, 0, 0), 4096);
        assert_eq!(queue_memory_size(0, 0, 0), 0);
    }

    #[test]
    fn set_defaults_keeps_the_device_and_sets_the_sizes() {
        let b = testing::alloc(crate::nvn::abi::size::QUEUE_BUILDER);
        builder_set_device(b, 0x1234);
        builder_set_queue_memory(b, 0x5000, 0x100);
        builder_set_defaults(b);
        assert_eq!(Obj(b).u64(B_DEV).expect("dev"), 0x1234);
        assert_eq!(Obj(b).u64(B_MEM).expect("mem"), 0);
        assert_eq!(builder_get_queue_memory_size(b), 344064);
    }

    #[test]
    fn the_group_exports_every_name_of_its_row() {
        let got: std::collections::HashSet<&str> = procs().iter().map(|(n, _)| *n).collect();
        let want: std::collections::HashSet<&str> = nxkit_nvn::entries::FFI_GROUPS
            .iter()
            .filter(|(g, _)| *g == "queue")
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        assert_eq!(got, want);
    }
}
