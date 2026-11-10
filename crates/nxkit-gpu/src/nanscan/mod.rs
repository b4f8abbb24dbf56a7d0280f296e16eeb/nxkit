//! The `nanscan` diagnostic. Counting kernels run over the float colour targets of the render passes and count the
//! texels that hold a NaN or an Inf in any channel. The counters land in one shared buffer. After the frame completed,
//! `dump` prints a line for each target with a count above 0. It runs on every frame while the category is on, not
//! only on sampled frames.
//!
//! `NXKIT_DIAG_FS` narrows it to the passes that hold a draw of one of the listed fragment programs. Without the
//! variable every pass counts. For such a pass `nanscan` also
//!
//! - counts a target that the pass loads before the pass (`at load`), so a line says whether the pass made the
//!   values; with an empty filter the knob `nanload` does it for every pass,
//! - counts the float textures that the listed programs sample, once per frame for each texture (`samples`), unless
//!   the pass renders to them,
//! - prints the fragment and mesh programs of the pass once per run (`pass programs`).
//!
//! The kernels (`kernel.rs`) read a texel with `read()` and test the exponent bits of the float they get, so they
//! cover `Rgba16Float`, `Rg16Float`, `R16Float`, `Rgba32Float`, `Rg32Float`, `R32Float` and `Rg11B10Float`, in 2D,
//! 2D array, 3D, cube and cube array textures (a cube is read as an array of its faces through a view). A texture
//! that stays uncovered (memoryless, without shader read, a view that Metal refuses) prints one note.

mod kernel;
mod text;

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Mutex;
use std::sync::atomic::AtomicU32;

use nxkit_metal::{
    ArgumentTable, CommandBuffer, ComputeCommand, Context, DataBuffer, Dependency, Format,
    LoadAction as MLoad, RenderPassDesc, Scope, Stages, Storage, Texture, TextureKind, Use,
    ViewDesc, Visibility,
};
use nxkit_nvn::passes::{Pass, Slot, StoreAction};
use nxkit_nvn::resource::TexId;

use crate::detail::Stage;
use crate::draws::Shared;
use kernel::{ENTRY_2D, ENTRY_3D, ENTRY_ARRAY, LIBRARY, MSL, WORDS};
use text::{Counts, Subject};

/// The records of one frame. Targets past this are not counted.
const SLOTS: usize = 384;
/// The lines of each kind (targets, samples) of one frame.
const LINES: usize = 40;
/// The fragment program keys that a line names.
const KEYS: usize = 6;
/// The distinct program sets that are listed once.
const PROGRAM_SETS: usize = 200;

/// What a record counts.
enum Kind {
    /// A stored target after the pass. `load` is the record of the same target before the pass.
    Target { load: Option<usize> },
    /// A target before the pass. Only the `Target` line prints it.
    Load,
    /// A texture that a program of the pass samples.
    Sample { stage: &'static str, slot: u32 },
}

/// One counted texture.
struct Rec {
    slot: usize,
    pass: usize,
    prog: String,
    subject: Subject,
    kind: Kind,
}

struct State {
    buffer: DataBuffer,
    next: usize,
    recs: Vec<Rec>,
    /// The textures of the frame that were counted as sampled.
    seen: HashSet<TexId>,
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
            seen: HashSet::new(),
        });
    }
    let Some(s) = st.as_mut() else { return false };
    s.next = 0;
    s.recs.clear();
    s.seen.clear();
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

/// The array layers of a texture of `kind` as the array kernel sees it: a cube is its six faces.
fn layers_of(kind: TextureKind, array_layers: u32) -> u32 {
    match kind {
        TextureKind::Cube => 6,
        TextureKind::CubeArray => 6 * array_layers,
        _ => array_layers,
    }
}

/// The texture the kernel reads, its entry, the threads of the grid and the depth for the line (`None` for 2D). The
/// text is the reason when the texture cannot be counted.
type Planned = (Texture, &'static str, [u32; 3], Option<u32>);

fn plan_scan(tex: &Texture) -> Result<Planned, String> {
    let d = tex.desc();
    if d.storage == Storage::Memoryless {
        return Err("memoryless".to_owned());
    }
    if !d.shader_read {
        return Err(format!("{:?} without shader read", d.kind));
    }
    let layers = layers_of(d.kind, d.array_layers);
    match d.kind {
        TextureKind::D2 => Ok((tex.clone(), ENTRY_2D, [d.width, d.height, 1], None)),
        TextureKind::D2Array => Ok((
            tex.clone(),
            ENTRY_ARRAY,
            [d.width, d.height, layers],
            Some(layers),
        )),
        TextureKind::D3 => Ok((
            tex.clone(),
            ENTRY_3D,
            [d.width, d.height, d.depth],
            Some(d.depth),
        )),
        TextureKind::Cube | TextureKind::CubeArray => {
            let view = tex
                .view(&ViewDesc {
                    format: d.format,
                    kind: TextureKind::D2Array,
                    first_level: 0,
                    level_count: 1,
                    first_slice: 0,
                    slice_count: layers,
                })
                .map_err(|e| format!("{:?} as an array: {e}", d.kind))?;
            Ok((view, ENTRY_ARRAY, [d.width, d.height, layers], Some(layers)))
        }
    }
}

/// The fragment and mesh program keys of the draws of a pass, each in order of first use, and whether a fragment
/// program is listed in `NXKIT_DIAG_FS`.
struct Programs {
    fragment: Vec<String>,
    mesh: Vec<String>,
    hit: bool,
}

fn programs(sh: &Shared<'_>, range: Range<u32>) -> Programs {
    let listed = crate::diag::fs_programs();
    let mut p = Programs {
        fragment: Vec::new(),
        mesh: Vec::new(),
        hit: false,
    };
    for n in range {
        let Some(d) = sh.detail.draws.get(n as usize) else {
            continue;
        };
        if let Some(key) = d.program.mesh.as_deref()
            && !p.mesh.iter().any(|k| k == key)
        {
            p.mesh.push(key.to_owned());
        }
        let Some(key) = d.program.fragment.as_deref() else {
            continue;
        };
        p.hit |= listed.iter().any(|l| l == key);
        if !p.fragment.iter().any(|k| k == key) {
            p.fragment.push(key.to_owned());
        }
    }
    p
}

/// Prints the programs of a pass the first time this set shows up.
fn announce(p: &Programs) {
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let lines = text::programs_lines(&p.fragment, &p.mesh);
    let Ok(mut seen) = SEEN.lock() else { return };
    if seen.len() >= PROGRAM_SETS || seen.contains(&lines[0]) && seen.contains(&lines[1]) {
        return;
    }
    seen.push(lines[0].clone());
    seen.push(lines[1].clone());
    for l in lines {
        eprintln!("{l}");
    }
}

/// Encodes the kernel over `tex` and records what it counts. `None` when the texture cannot be counted.
fn scan_texture(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    index: usize,
    id: TexId,
    tex: &Texture,
    prog: &str,
    kind: Kind,
) -> Option<usize> {
    let d = tex.desc();
    let (view, entry, grid, depth) = match plan_scan(tex) {
        Ok(p) => p,
        Err(why) => {
            static NOTED: AtomicU32 = AtomicU32::new(0);
            if crate::diag::first(&NOTED, 10) {
                eprintln!(
                    "DIAG nanscan pass {index} texture {id:?} {:?} {}x{} not counted: {why}",
                    d.format, d.width, d.height
                );
            }
            return None;
        }
    };
    let pipeline = sh.pipelines.host_compute(LIBRARY, MSL, entry).ok()?;
    let table = ArgumentTable::new(sh.ctx, 1, 1).ok()?;
    let (slot, addr) = {
        let mut st = STATE.lock().ok()?;
        let s = st.as_mut()?;
        if s.next >= SLOTS {
            return None;
        }
        let slot = s.next;
        s.next += 1;
        s.recs.push(Rec {
            slot,
            pass: index,
            prog: prog.to_owned(),
            subject: Subject {
                id,
                format: d.format,
                size: [d.width, d.height],
                depth,
            },
            kind,
        });
        (slot, s.buffer.gpu_address() + (slot * WORDS * 4) as u64)
    };
    let mut cp = cb.begin_compute_pass(&table).ok()?;
    let _ = cp.encode(&ComputeCommand::WaitForEarlierPasses);
    let _ = cp.encode(&ComputeCommand::Depend(Dependency {
        after: Stages::ALL,
        ..Dependency::between(Use::AttachmentWrite, Use::ComputeRead)
    }));
    let _ = cp.encode(&ComputeCommand::SetPipeline(pipeline));
    let _ = cp.encode(&ComputeCommand::SetTexture {
        index: 0,
        texture: view,
    });
    let _ = cp.encode(&ComputeCommand::SetBuffer {
        index: 0,
        gpu_addr: addr,
    });
    let _ = cp.encode(&ComputeCommand::DispatchThreads {
        grid,
        threads: [8, 8, 1],
    });
    // A later pass may write the texture again; it waits for the kernel.
    let _ = cp.encode(&ComputeCommand::Depend(Dependency {
        scope: Scope::ToLaterPasses,
        after: Stages::DISPATCH,
        before: Stages::ALL,
        visibility: Visibility::Device,
    }));
    cp.end();
    Some(slot)
}

/// A stored float colour target of a pass.
struct Stored {
    id: TexId,
    tex: Texture,
    /// The pass loads the target.
    loads: bool,
    /// The record of the count before the pass.
    load: Option<usize>,
}

/// What `before` found for a pass and `after` needs.
pub(crate) struct Scan {
    pass: usize,
    prog: String,
    stored: Vec<Stored>,
}

/// Encodes what runs before the render pass `index`: the count of the loaded targets and of the sampled textures.
/// `draws` is the draw range of the pass and `desc` the pass as Metal gets it. None when the pass is not scanned.
pub(crate) fn before(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    index: usize,
    pass: &Pass,
    draws: Range<u32>,
    desc: &RenderPassDesc,
) -> Option<Scan> {
    if !active() {
        return None;
    }
    let mut attached = Vec::new();
    let mut stored = Vec::new();
    for a in sh.plan.attachments_of(pass) {
        attached.push(a.tex);
        let Slot::Color(slot) = a.slot else { continue };
        let Some(Some(color)) = desc.colors.get(usize::from(slot)) else {
            continue;
        };
        if a.store == StoreAction::Store && float_format(color.texture.desc().format) {
            stored.push(Stored {
                id: a.tex,
                tex: color.texture.clone(),
                loads: color.load == MLoad::Load,
                load: None,
            });
        }
    }
    if stored.is_empty() {
        return None;
    }
    let progs = programs(sh, draws.clone());
    let filtered = !crate::diag::fs_programs().is_empty();
    if filtered && !progs.hit {
        return None;
    }
    announce(&progs);
    let prog = if progs.fragment.is_empty() {
        "-".to_owned()
    } else {
        progs
            .fragment
            .iter()
            .take(KEYS)
            .cloned()
            .collect::<Vec<_>>()
            .join(",")
    };
    if filtered || crate::diag::knob("nanload") {
        for s in stored.iter_mut().filter(|s| s.loads) {
            s.load = scan_texture(sh, cb, index, s.id, &s.tex, &prog, Kind::Load);
        }
    }
    if filtered {
        samples(sh, cb, index, draws, &attached);
    }
    Some(Scan {
        pass: index,
        prog,
        stored,
    })
}

/// Counts the float textures that the listed programs of the pass sample, once per frame for each texture. A texture
/// that the pass renders to is left out.
fn samples(
    sh: &Shared<'_>,
    cb: &mut CommandBuffer,
    index: usize,
    draws: Range<u32>,
    attached: &[TexId],
) {
    let listed = crate::diag::fs_programs();
    let barrier_ids: Vec<TexId> = attached
        .iter()
        .filter_map(|id| sh.textures.get(id).map(|r| r.barrier_id))
        .collect();
    for n in draws {
        let Some(d) = sh.detail.draws.get(n as usize) else {
            continue;
        };
        let Some(key) = d.program.fragment.as_deref() else {
            continue;
        };
        if !listed.iter().any(|l| l == key) {
            continue;
        }
        for t in d.textures.iter().filter(|t| t.handle != 0) {
            let stage = match t.stage {
                Stage::Fragment => "fs",
                Stage::Mesh => "ms",
                Stage::Task => "ts",
                Stage::Vertex => "vs",
                Stage::Compute => continue,
            };
            let Some(id) = crate::barr::texture_of(t.handle) else {
                continue;
            };
            let Some(res) = sh.textures.get(&id) else {
                continue;
            };
            if attached.contains(&id)
                || barrier_ids.contains(&res.barrier_id)
                || !float_format(res.desc.format)
            {
                continue;
            }
            let first = STATE
                .lock()
                .ok()
                .and_then(|mut st| st.as_mut().map(|s| s.seen.insert(id)))
                .unwrap_or(false);
            if first {
                let kind = Kind::Sample {
                    stage,
                    slot: t.slot,
                };
                scan_texture(sh, cb, index, id, &res.tex, key, kind);
            }
        }
    }
}

impl Scan {
    /// Encodes the count of the stored targets, right after the render pass.
    pub(crate) fn after(self, sh: &Shared<'_>, cb: &mut CommandBuffer) {
        for s in self.stored {
            scan_texture(
                sh,
                cb,
                self.pass,
                s.id,
                &s.tex,
                &self.prog,
                Kind::Target { load: s.load },
            );
        }
    }
}

/// Prints the textures with non-finite texels in pass order. The GPU has to be done with the frame.
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
    let counts = |slot: usize| Counts::from_words(&words[slot * WORDS..(slot + 1) * WORDS]);
    let frame = crate::diag::frame();
    let (mut targets, mut samples) = (0, 0);
    for r in recs {
        let c = counts(r.slot);
        if c.total == 0 {
            continue;
        }
        match r.kind {
            Kind::Load => {}
            Kind::Target { load } if targets < LINES => {
                targets += 1;
                let at_load = load.map(|l| words[l * WORDS]);
                eprintln!(
                    "{}",
                    text::target_line(frame, r.pass, &r.prog, &r.subject, &c, at_load)
                );
            }
            Kind::Sample { stage, slot } if samples < LINES => {
                samples += 1;
                eprintln!(
                    "{}",
                    text::sample_line(frame, r.pass, &r.prog, &r.subject, stage, slot, &c)
                );
            }
            _ => {}
        }
    }
    s.next = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cube_is_read_as_an_array_of_its_faces() {
        assert_eq!(layers_of(TextureKind::Cube, 6), 6);
        assert_eq!(layers_of(TextureKind::Cube, 1), 6);
        assert_eq!(layers_of(TextureKind::CubeArray, 3), 18);
        assert_eq!(layers_of(TextureKind::D2Array, 5), 5);
        assert_eq!(layers_of(TextureKind::D3, 1), 1);
    }

    #[test]
    fn the_kernels_cover_the_float_formats_only() {
        for f in [
            Format::Rgba16Float,
            Format::Rg16Float,
            Format::R16Float,
            Format::Rgba32Float,
            Format::Rg32Float,
            Format::R32Float,
            Format::Rg11B10Float,
        ] {
            assert!(float_format(f), "{f:?}");
        }
        for f in [
            Format::Rgba8Unorm,
            Format::Depth32Float,
            Format::Rgb9E5Float,
        ] {
            assert!(!float_format(f), "{f:?}");
        }
    }
}
