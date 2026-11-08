//! The life of a command buffer: initialise, memory, callback, recording, call and copy, and the getters.

use nxkit_nvn::record::Record;

use super::rec::{
    self, MAGIC, O_IMAGE_STRIDE, O_MAGIC, O_REC, O_ROW_STRIDE, create, destroy, sync_in, sync_out,
    with_recorder,
};
use crate::nvn::abi::{Obj, arg_bool, guard, other, ret_bool, table, used};
use crate::nvn::pool;

/// How many handles one `CallCommands` takes at most. A game that passes more is wrong.
const MAX_CALLED: i32 = 1024;

extern "C" fn initialize(c: u64, _dev: u64) -> u32 {
    guard("nvnCommandBufferInitialize", || {
        Obj(c).zero(used::COMMAND_BUFFER)?;
        Obj(c).set_u32(O_MAGIC, MAGIC)?;
        create(c);
        Ok(1)
    })
}

extern "C" fn finalize(c: u64) {
    guard("nvnCommandBufferFinalize", || {
        destroy(c);
        Obj(c).set_u32(O_REC, 0)?;
        Obj(c).set_u32(O_MAGIC, 0)
    });
}

extern "C" fn add_command_memory(c: u64, memory_pool: u64, off: i64, n: u64) {
    guard("nvnCommandBufferAddCommandMemory", || {
        let v = pool::view(memory_pool)?;
        let off = u64::try_from(off).map_err(|_| other("the command memory offset is negative"))?;
        if v.mem == 0 || v.cpu_no_access() {
            return Err(other("the pool of the command memory has no CPU mapping"));
        }
        if off.checked_add(n).is_none_or(|end| end > v.size) {
            return Err(other("the command memory is not inside its pool"));
        }
        let host = v.mem + off;
        super::seam::validate_region(host, n)?;
        with_recorder(c, |r| {
            sync_in(c, r)?;
            r.buf.add_command_memory(host, n)?;
            sync_out(c, r)
        })
    });
}

extern "C" fn add_control_memory(c: u64, _mem: u64, n: u64) {
    guard("nvnCommandBufferAddControlMemory", || {
        with_recorder(c, |r| {
            r.control = r.control.saturating_add(n);
            Ok(())
        })
    });
}

extern "C" fn set_memory_callback(c: u64, f: u64) {
    guard("nvnCommandBufferSetMemoryCallback", || {
        with_recorder(c, |r| {
            r.hook.func = f;
            r.hook.enabled = f != 0;
            Ok(())
        })
    });
}

extern "C" fn set_memory_callback_data(c: u64, d: u64) {
    guard("nvnCommandBufferSetMemoryCallbackData", || {
        with_recorder(c, |r| {
            r.hook.data = d;
            Ok(())
        })
    });
}

extern "C" fn set_callback_enabled(c: u64, on: u32) {
    guard("nvnCommandBufferSetCommandMemoryCallbackEnabled", || {
        with_recorder(c, |r| {
            r.hook.enabled = arg_bool(on);
            Ok(())
        })
    });
}

extern "C" fn begin_recording(c: u64) {
    guard("nvnCommandBufferBeginRecording", || {
        with_recorder(c, |r| {
            sync_in(c, r)?;
            r.buf.begin_recording()?;
            sync_out(c, r)
        })?;
        Obj(c).set_u32(O_REC, 1)
    });
}

extern "C" fn end_recording(c: u64) -> u64 {
    rec::end("nvnCommandBufferEndRecording", c)
}

extern "C" fn is_recording(c: u64) -> u32 {
    guard("nvnCommandBufferIsRecording", || {
        Ok(ret_bool(Obj(c).u32(O_REC)? != 0))
    })
}

/// `CallCommands` and `CopyCommands`: each handle becomes a call record. The called memory must stay as it is until
/// the submit (unverified for `CopyCommands`, which on NVN copies the commands).
fn call(name: &'static str, c: u64, n: i32, handles: u64) {
    guard(name, || {
        if !(0..=MAX_CALLED).contains(&n) {
            return Err(other(format!("{n} command handles are not valid")));
        }
        for i in 0..n as u64 {
            let h = Obj(handles + i * 8).u64(0)?;
            rec::emit_rec(name, c, Record::Call(h));
        }
        Ok(())
    });
}

extern "C" fn call_commands(c: u64, n: i32, handles: u64) {
    call("nvnCommandBufferCallCommands", c, n, handles);
}

extern "C" fn copy_commands(c: u64, n: i32, handles: u64) {
    call("nvnCommandBufferCopyCommands", c, n, handles);
}

fn memory(name: &'static str, c: u64, pick: fn(u64, u64, u64) -> u64) -> u64 {
    guard(name, || {
        with_recorder(c, |r| {
            sync_in(c, r)?;
            Ok(pick(r.buf.used(), r.buf.free(), r.control))
        })
    })
}

extern "C" fn command_used(c: u64) -> u64 {
    memory("nvnCommandBufferGetCommandMemoryUsed", c, |u, _, _| u)
}

extern "C" fn command_free(c: u64) -> u64 {
    memory("nvnCommandBufferGetCommandMemoryFree", c, |_, f, _| f)
}

extern "C" fn command_size(c: u64) -> u64 {
    memory("nvnCommandBufferGetCommandMemorySize", c, |u, f, _| u + f)
}

extern "C" fn control_used(c: u64) -> u64 {
    memory("nvnCommandBufferGetControlMemoryUsed", c, |_, _, _| 0)
}

extern "C" fn control_free(c: u64) -> u64 {
    memory("nvnCommandBufferGetControlMemoryFree", c, |_, _, k| k)
}

extern "C" fn control_size(c: u64) -> u64 {
    memory("nvnCommandBufferGetControlMemorySize", c, |_, _, k| k)
}

extern "C" fn get_copy_row_stride(c: u64) -> i64 {
    guard("nvnCommandBufferGetCopyRowStride", || Obj(c).i64(O_ROW_STRIDE))
}

extern "C" fn get_copy_image_stride(c: u64) -> i64 {
    guard("nvnCommandBufferGetCopyImageStride", || Obj(c).i64(O_IMAGE_STRIDE))
}

/// The functions that NVN defines for a hint that the host has no use for. Nothing is recorded.
extern "C" fn hint(_c: u64) {}

/// The names of the hint functions and the functions that nxnative answers with an empty body.
const HINTS: &[&str] = &[
    "nvnCommandBufferSetPatchSize",
    "nvnCommandBufferSetDepthBounds",
    "nvnCommandBufferWaitSync",
    "nvnCommandBufferSetShaderScratchMemory",
    "nvnCommandBufferPushDebugGroupDynamic",
    "nvnCommandBufferPopDebugGroup",
    "nvnCommandBufferInsertDebugMarkerDynamic",
    "nvnCommandBufferInsertDebugMarker",
    "nvnCommandBufferPushDebugGroup",
    "nvnCommandBufferPushDebugGroupStatic",
    "nvnCommandBufferPopDebugGroupId",
    "nvnCommandBufferInsertDebugMarkerStatic",
    "nvnCommandBufferWaitEvent",
    "nvnCommandBufferSetStencilCullCriteria",
    "nvnCommandBufferSaveZCullData",
    "nvnCommandBufferRestoreZCullData",
    "nvnCommandBufferSetZCullZF32CompressionEnable",
    "nvnCommandBufferSetColorReductionEnable",
    "nvnCommandBufferSetColorReductionThresholds",
    "nvnCommandBufferSetRecordingMode",
    "nvnCommandBufferSetGeometryProcessingMode",
    "nvnCommandBufferSetComputeWorkloadPriority",
    "nvnCommandBufferSetComputeOverlapMode",
    "nvnCommandBufferShadingRateImagePalette",
    "nvnCommandBufferSetShadingRatePaletteMode",
    "nvnCommandBufferBindShadingRateImage",
    "nvnCommandBufferSetDebugLabel",
];

pub(super) fn procs() -> Vec<(&'static str, usize)> {
    let mut v = table![
        "nvnCommandBufferInitialize" => initialize as extern "C" fn(u64, u64) -> u32,
        "nvnCommandBufferFinalize" => finalize as extern "C" fn(u64),
        "nvnCommandBufferAddCommandMemory" => add_command_memory as extern "C" fn(u64, u64, i64, u64),
        "nvnCommandBufferAddControlMemory" => add_control_memory as extern "C" fn(u64, u64, u64),
        "nvnCommandBufferSetMemoryCallback" => set_memory_callback as extern "C" fn(u64, u64),
        "nvnCommandBufferSetMemoryCallbackData" => set_memory_callback_data as extern "C" fn(u64, u64),
        "nvnCommandBufferSetCommandMemoryCallbackEnabled" => set_callback_enabled as extern "C" fn(u64, u32),
        "nvnCommandBufferBeginRecording" => begin_recording as extern "C" fn(u64),
        "nvnCommandBufferEndRecording" => end_recording as extern "C" fn(u64) -> u64,
        "nvnCommandBufferIsRecording" => is_recording as extern "C" fn(u64) -> u32,
        "nvnCommandBufferCallCommands" => call_commands as extern "C" fn(u64, i32, u64),
        "nvnCommandBufferCopyCommands" => copy_commands as extern "C" fn(u64, i32, u64),
        "nvnCommandBufferGetCommandMemoryUsed" => command_used as extern "C" fn(u64) -> u64,
        "nvnCommandBufferGetCommandMemoryFree" => command_free as extern "C" fn(u64) -> u64,
        "nvnCommandBufferGetCommandMemorySize" => command_size as extern "C" fn(u64) -> u64,
        "nvnCommandBufferGetControlMemoryUsed" => control_used as extern "C" fn(u64) -> u64,
        "nvnCommandBufferGetControlMemoryFree" => control_free as extern "C" fn(u64) -> u64,
        "nvnCommandBufferGetControlMemorySize" => control_size as extern "C" fn(u64) -> u64,
        "nvnCommandBufferGetCopyRowStride" => get_copy_row_stride as extern "C" fn(u64) -> i64,
        "nvnCommandBufferGetCopyImageStride" => get_copy_image_stride as extern "C" fn(u64) -> i64,
    ];
    let nop: extern "C" fn(u64) = hint;
    v.extend(HINTS.iter().map(|n| (*n, nop as usize)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvn::abi::size;
    use crate::nvn::abi::testing::alloc;

    #[test]
    fn an_initialised_command_buffer_is_not_recording_and_has_no_memory() {
        let c = alloc(size::COMMAND_BUFFER);
        assert_eq!(initialize(c, 0), 1);
        assert_eq!(is_recording(c), 0);
        assert_eq!(command_size(c), 0);
        assert_eq!(control_size(c), 0);
        add_control_memory(c, 0, 4096);
        assert_eq!(control_free(c), 4096);
        assert_eq!(control_used(c), 0);
        finalize(c);
    }

    #[test]
    fn the_copy_strides_are_kept() {
        let c = alloc(size::COMMAND_BUFFER);
        initialize(c, 0);
        Obj(c).set_i64(O_ROW_STRIDE, 256).unwrap();
        Obj(c).set_i64(O_IMAGE_STRIDE, 65536).unwrap();
        assert_eq!((get_copy_row_stride(c), get_copy_image_stride(c)), (256, 65536));
    }

    #[test]
    fn the_hint_names_are_all_distinct() {
        let mut v: Vec<_> = HINTS.to_vec();
        v.sort_unstable();
        v.dedup();
        assert_eq!(v.len(), HINTS.len());
    }
}
