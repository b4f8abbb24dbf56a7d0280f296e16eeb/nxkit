//! The `nanscan` diagnostic. After each render pass that stores a colour attachment of a float format, one counting
//! kernel runs over the texture and counts the texels that hold a NaN or an Inf in any channel. The counters land in
//! one shared buffer. After the frame completed, `dump` prints a line for each pass with a count above 0. It runs on
//! every frame while the category is on, not only on sampled frames.
//!
//! `NXKIT_DIAG_FS` narrows it to the passes that hold a draw of one of the listed fragment programs. Without the
//! variable every pass counts.
//!
//! The kernel reads the texture with `read()` and tests the exponent bits of the float it gets: a half, a 32-bit
//! float or a field of `Rg11B10Float` with an all-ones exponent becomes a float with exponent 255. The test is on
//! the bits (`as_type<uint>`), because the compiler may assume that a float is finite. It covers `Rgba16Float`,
//! `Rg16Float`, `R16Float`, `Rgba32Float`, `Rg32Float`, `R32Float` and `Rg11B10Float` of plain 2D targets that
//! shaders may read. A texture without shader read, an array, a cube or a volume is skipped, with a note.

use std::ops::Range;
use std::sync::Mutex;
use std::sync::atomic::AtomicU32;

use nxkit_metal::{
    ArgumentTable, CommandBuffer, ComputeCommand, Context, DataBuffer, Dependency, Format, Scope,
    Stages, Storage, Texture, TextureKind, Use, Visibility,
};
use nxkit_nvn::resource::TexId;

use crate::draws::Shared;

const MSL: &str = r"
#include <metal_stdlib>
using namespace metal;

// The record of one target: the count, then the x and y of the first 8 texels.
kernel void nxkit_nanscan(
    texture2d<float, access::read> t [[texture(0)]],
    device atomic_uint* rec [[buffer(0)]],
    uint2 gid [[thread_position_in_grid]])
{
    if (gid.x >= t.get_width() || gid.y >= t.get_height()) {
        return;
    }
    uint4 bits = as_type<uint4>(t.read(gid));
    uint4 exponent = bits & 0x7f800000u;
    if (any(exponent == 0x7f800000u)) {
        uint n = atomic_fetch_add_explicit(&rec[0], 1u, memory_order_relaxed);
        if (n < 8u) {
            atomic_store_explicit(&rec[1u + 2u * n], gid.x, memory_order_relaxed);
            atomic_store_explicit(&rec[2u + 2u * n], gid.y, memory_order_relaxed);
        }
    }
}
";

/// The words of one record: the count and 8 coordinate pairs.
const WORDS: usize = 17;
/// The records of one frame. Targets past this are not counted.
const SLOTS: usize = 256;
/// The lines of one frame.
const LINES: usize = 40;
/// The fragment program keys that a line names.
const KEYS: usize = 6;

/// One counted target.
struct Rec {
    slot: usize,
    pass: usize,
    prog: String,
    id: TexId,
    format: Format,
    size: [u32; 2],
}

struct State {
    buffer: DataBuffer,
    next: usize,
    recs: Vec<Rec>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// True while the category is on.
pub(crate) fn active() -> bool {
    crate::diag("nanscan")
}

/// Starts a frame: the counters are zero and the list is empty. The caller has waited for the GPU to finish the
/// frame before. False when the category is off.
pub(crate) fn begin(ctx: &Context) -> bool {
    if !active() {
        return false;
    }
    let Ok(mut st) = STATE.lock() else {
        return false;
    };
    if st.is_none() {
        let Ok(buffer) = DataBuffer::new(ctx, (SLOTS * WORDS * 4) as u64) else {
            return false;
        };
        *st = Some(State {
            buffer,
            next: 0,
            recs: Vec::new(),
        });
    }
    let Some(s) = st.as_mut() else { return false };
    s.next = 0;
    s.recs.clear();
    s.buffer.write(0, &vec![0u8; SLOTS * WORDS * 4]).is_ok()
}

fn float_format(f: Format) -> bool {
    matches!(
        f,
        Format::Rgba16Float
            | Format::Rg16Float
            | Format::R16Float
            | Format::Rgba32Float
            | Format::Rg32Float
            | Format::R32Float
            | Format::Rg11B10Float
    )
}

/// The fragment program keys of the draws of a pass, in order of first use, and whether one of them is listed in
/// `NXKIT_DIAG_FS`.
fn programs(sh: &Shared<'_>, range: Range<u32>) -> (Vec<String>, bool) {
    let listed = crate::diag::fs_programs();
    let mut keys: Vec<String> = Vec::new();
    let mut hit = false;
    for n in range {
        let Some(key) = sh
            .detail
            .draws
            .get(n as usize)
            .and_then(|d| d.program.fragment.as_deref())
        else {
            continue;
        };
        hit |= listed.iter().any(|l| l == key);
        if !keys.iter().any(|k| k == key) {
            keys.push(key.to_owned());
        }
    }
    (keys, hit)
}

/// Encodes the counting kernels for the stored colour attachments of the pass `index`, right after its render pass.
/// `draws` is the draw range of the pass.
pub(crate) fn pass(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    index: usize,
    draws: Range<u32>,
    stored: &[(TexId, &Texture)],
) {
    if !stored.iter().any(|(_, t)| float_format(t.desc().format)) {
        return;
    }
    let (keys, hit) = programs(sh, draws);
    if !crate::diag::fs_programs().is_empty() && !hit {
        return;
    }
    let prog = if keys.is_empty() {
        "-".to_owned()
    } else {
        keys.iter()
            .take(KEYS)
            .cloned()
            .collect::<Vec<_>>()
            .join(",")
    };
    for (id, tex) in stored {
        let d = tex.desc();
        if !float_format(d.format) {
            continue;
        }
        if d.storage == Storage::Memoryless
            || !d.shader_read
            || d.kind != TextureKind::D2
            || d.array_layers != 1
        {
            static NOTED: AtomicU32 = AtomicU32::new(0);
            if crate::diag::first(&NOTED, 10) {
                eprintln!(
                    "DIAG nanscan pass {index} target {id:?} {:?} {}x{} not counted: {:?} shader read {}",
                    d.format, d.width, d.height, d.kind, d.shader_read
                );
            }
            continue;
        }
        let Ok(pipeline) = sh
            .pipelines
            .host_compute("nxkit-nanscan", MSL, "nxkit_nanscan")
        else {
            return;
        };
        let Ok(table) = ArgumentTable::new(sh.ctx, 1, 1) else {
            return;
        };
        let addr = {
            let Ok(mut st) = STATE.lock() else { return };
            let Some(s) = st.as_mut() else { return };
            if s.next >= SLOTS {
                return;
            }
            let slot = s.next;
            s.next += 1;
            s.recs.push(Rec {
                slot,
                pass: index,
                prog: prog.clone(),
                id: *id,
                format: d.format,
                size: [d.width, d.height],
            });
            s.buffer.gpu_address() + (slot * WORDS * 4) as u64
        };
        let Ok(mut cp) = cb.begin_compute_pass(&table) else {
            return;
        };
        let _ = cp.encode(&ComputeCommand::Depend(Dependency {
            after: Stages::ALL,
            ..Dependency::between(Use::AttachmentWrite, Use::ComputeRead)
        }));
        let _ = cp.encode(&ComputeCommand::SetPipeline(pipeline));
        let _ = cp.encode(&ComputeCommand::SetTexture {
            index: 0,
            texture: (*tex).clone(),
        });
        let _ = cp.encode(&ComputeCommand::SetBuffer {
            index: 0,
            gpu_addr: addr,
        });
        let _ = cp.encode(&ComputeCommand::DispatchThreads {
            grid: [d.width, d.height, 1],
            threads: [8, 8, 1],
        });
        // A later pass may write the target again; it waits for the kernel.
        let _ = cp.encode(&ComputeCommand::Depend(Dependency {
            scope: Scope::ToLaterPasses,
            after: Stages::DISPATCH,
            before: Stages::ALL,
            visibility: Visibility::Device,
        }));
        cp.end();
    }
}

/// Prints the targets with non-finite texels in pass order. The GPU has to be done with the frame.
pub(crate) fn dump() {
    let Ok(mut st) = STATE.lock() else { return };
    let Some(s) = st.as_mut() else { return };
    let mut recs = std::mem::take(&mut s.recs);
    recs.sort_by_key(|r| (r.pass, r.slot));
    let mut bytes = vec![0u8; SLOTS * WORDS * 4];
    if s.buffer.read(0, &mut bytes).is_err() {
        return;
    }
    let words: Vec<u32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| u32::from_le_bytes(*b))
        .collect();
    let frame = crate::diag::frame();
    let mut printed = 0;
    for r in recs {
        let w = &words[r.slot * WORDS..(r.slot + 1) * WORDS];
        let count = w[0];
        if count == 0 {
            continue;
        }
        if printed == LINES {
            break;
        }
        printed += 1;
        let first: Vec<String> = (0..count.min(8) as usize)
            .map(|i| format!("({},{})", w[1 + 2 * i], w[2 + 2 * i]))
            .collect();
        eprintln!(
            "DIAG nanscan frame {frame} pass {} prog {} target {:?} {:?} {}x{} nonfinite {count} first {}",
            r.pass,
            r.prog,
            r.id,
            r.format,
            r.size[0],
            r.size[1],
            first.join(" ")
        );
    }
    s.next = 0;
}
