//! Counters of one encoded frame (R16) and the report that the caller applies to its tables.

use nxkit_nvn::resource::TexId;

use crate::detail::DrawArgs;

/// Why a draw, copy or dispatch was not encoded. The encoder counts each reason, so a missing feature shows up as a
/// number and not as a silent hole in the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Skip {
    /// The frame has no detail for the command.
    Detail,
    /// The shader library or the reflection is missing or not readable.
    Program,
    /// The reflection has a binding kind that the encoder does not fill.
    Binding,
    /// The primitive type has no Metal equivalent (fan, quads, adjacency, patches).
    Primitive,
    /// The index type has no Metal equivalent (byte indices).
    Index,
    /// A vertex or texture format has no Metal equivalent.
    Format,
    /// The render targets leave a gap between colour slots, or the pass has no target.
    Target,
    /// Front and back faces are both culled.
    Cull,
    /// A texture could not be created or resolved.
    Texture,
    /// The pipeline did not compile.
    Pipeline,
    /// A buffer address is outside guest memory, or the scratch buffer is full.
    Buffer,
    /// A copy between a scaled and an unscaled texture, or between different formats.
    Copy,
    /// A texture upload or copy-back failed.
    Transfer,
    /// Metal refused an encoded command.
    Encode,
    /// The pipeline is still compiling on a background thread. The draw is left out until it is ready.
    Pending,
    /// The draw's program is listed in `NXKIT_SKIP_PROGRAMS` or `NXKIT_SKIP_FILE`.
    Listed,
}

impl Skip {
    /// The number of reasons.
    pub const COUNT: usize = 16;

    /// Every reason, in index order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Detail,
        Self::Program,
        Self::Binding,
        Self::Primitive,
        Self::Index,
        Self::Format,
        Self::Target,
        Self::Cull,
        Self::Texture,
        Self::Pipeline,
        Self::Buffer,
        Self::Copy,
        Self::Transfer,
        Self::Encode,
        Self::Pending,
        Self::Listed,
    ];

    /// The reason as a short name for logs.
    pub fn name(self) -> &'static str {
        match self {
            Self::Detail => "detail",
            Self::Program => "program",
            Self::Binding => "binding",
            Self::Primitive => "primitive",
            Self::Index => "index",
            Self::Format => "format",
            Self::Target => "target",
            Self::Cull => "cull",
            Self::Texture => "texture",
            Self::Pipeline => "pipeline",
            Self::Buffer => "buffer",
            Self::Copy => "copy",
            Self::Transfer => "transfer",
            Self::Encode => "encode",
            Self::Pending => "pending",
            Self::Listed => "listed",
        }
    }
}

/// The draws that the GPU drives or that run a mesh pipeline. [`KindStats`] counts them apart from the other draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawClass {
    /// `DrawMeshTasks`.
    Mesh,
    /// `DrawMeshTasksIndirect`.
    MeshIndirect,
    /// `DrawArraysIndirect` and `DrawElementsIndirect`.
    Indirect,
    /// `MultiDraw{Arrays,Elements}IndirectCount`.
    MultiIndirectCount,
}

impl DrawClass {
    /// The number of classes.
    pub const COUNT: usize = 4;

    /// The class of a draw, or `None` for a direct vertex draw.
    pub fn of(args: &DrawArgs) -> Option<Self> {
        Some(match args {
            DrawArgs::MeshTasks { .. } => Self::Mesh,
            DrawArgs::MeshTasksIndirect { .. } => Self::MeshIndirect,
            DrawArgs::ArraysIndirect { .. } | DrawArgs::IndexedIndirect { .. } => Self::Indirect,
            DrawArgs::MultiIndirectCount { .. } => Self::MultiIndirectCount,
            DrawArgs::Arrays { .. } | DrawArgs::Indexed { .. } => return None,
        })
    }

    /// The class as a short name for logs.
    pub fn name(self) -> &'static str {
        match self {
            Self::Mesh => "mesh",
            Self::MeshIndirect => "mesh-indirect",
            Self::Indirect => "indirect",
            Self::MultiIndirectCount => "multi-indirect-count",
        }
    }

    /// Every class, in index order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Mesh,
        Self::MeshIndirect,
        Self::Indirect,
        Self::MultiIndirectCount,
    ];
}

/// The mesh and indirect draws of a frame: how many were encoded and why the others were not. They are also in
/// [`EncodeStats::draws`] and [`EncodeStats::skipped`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KindStats {
    /// Encoded draws by class (index is `DrawClass as usize`).
    pub encoded: [u32; DrawClass::COUNT],
    /// Skipped draws by class and reason.
    pub skipped: [[u32; Skip::COUNT]; DrawClass::COUNT],
    /// The Metal draws that the encoded draws became: one per grid of a mesh draw and one per record of a multi-draw.
    pub metal_draws: u32,
    /// Compute pre-passes that copied indirect arguments (`indirect`).
    pub prepasses: u32,
    /// Records that the pre-passes copied.
    pub prepass_records: u32,
    /// Draws of multi-draws past the most that the pre-pass copies, and workgroups of mesh draws past the most grids.
    /// They are left out.
    pub clamped: u32,
}

impl KindStats {
    /// True when the frame had no mesh or indirect draw.
    pub fn is_empty(&self) -> bool {
        self.encoded.iter().all(|n| *n == 0) && self.skipped.iter().flatten().all(|n| *n == 0)
    }

    fn add(&mut self, o: &Self) {
        for (a, b) in self.encoded.iter_mut().zip(o.encoded) {
            *a += b;
        }
        for (a, b) in self.skipped.iter_mut().zip(o.skipped) {
            for (x, y) in a.iter_mut().zip(b) {
                *x += y;
            }
        }
        self.metal_draws += o.metal_draws;
        self.prepasses += o.prepasses;
        self.prepass_records += o.prepass_records;
        self.clamped += o.clamped;
    }

    /// One line per class with draws, for the `mesh` diagnostic: the encoded draws and the skipped ones by reason.
    pub fn describe(&self) -> String {
        let mut out = Vec::new();
        for c in DrawClass::ALL {
            let i = c as usize;
            let skipped: Vec<String> = self.skipped[i]
                .iter()
                .zip(Skip::ALL)
                .filter(|(n, _)| **n > 0)
                .map(|(n, why)| format!("{} {n}", why.name()))
                .collect();
            if self.encoded[i] > 0 || !skipped.is_empty() {
                out.push(format!(
                    "{} encoded {} skipped [{}]",
                    c.name(),
                    self.encoded[i],
                    skipped.join(", ")
                ));
            }
        }
        format!(
            "{}; metal draws {} pre-passes {} records {} clamped {}",
            out.join("; "),
            self.metal_draws,
            self.prepasses,
            self.prepass_records,
            self.clamped
        )
    }
}

/// What one `Gpu::encode_frame` did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EncodeStats {
    /// Draws that went into Metal commands, directly or through an ICB.
    pub draws: u32,
    /// Draws that were skipped, by reason (index is `Skip as usize`).
    pub skipped: [u32; Skip::COUNT],
    /// Draws that an ICB executed. They are also in `draws`.
    pub icb_draws: u32,
    /// The mesh and indirect draws, encoded and skipped by reason.
    pub kinds: KindStats,
    /// `executeCommandsInBuffer` calls.
    pub icb_executions: u32,
    /// ICBs encoded or encoded again this frame.
    pub icb_builds: u32,
    /// Times a render pass set another pipeline than the one before.
    pub pipeline_switches: u32,
    /// Pipelines compiled this frame.
    pub pipeline_builds: u32,
    /// Barriers between passes that were emitted.
    pub barriers_emitted: u32,
    /// Pass boundaries where no data flows and no barrier was emitted.
    pub barriers_avoided: u32,
    /// Render passes encoded.
    pub render_passes: u32,
    /// Compute passes encoded (dispatches, copies, uploads, copy-backs).
    pub compute_passes: u32,
    /// Command buffers in the batch.
    pub command_buffers: u32,
    /// Metal textures created or created again.
    pub textures_created: u32,
    /// Textures created again at another scale. Their old contents are lost.
    pub textures_rescaled: u32,
    /// Textures deswizzled from guest memory.
    pub uploads: u32,
    /// Textures swizzled back to guest memory: for a CPU read, or because the frame wrote a texture whose storage
    /// the CPU can access (`copyback`).
    pub copybacks: u32,
    /// Texel bytes that the copy-backs wrote into guest memory.
    pub copyback_bytes: u64,
    /// Texture views whose channel swizzle is not the identity. Metal views here carry no swizzle, so it is ignored.
    pub swizzle_ignored: u32,
    /// Textures that are scaled and had guest data that is not copied into them.
    pub scaled_upload_lost: u32,
    /// Scaled textures that got guest data through a native-size staging texture and a GPU scale.
    pub scaled_uploads: u32,
    /// Textures that stay at the native size because the CPU writes them often and the GPU renders them rarely.
    pub textures_kept_native: u32,
    /// Pitch-linear textures made over their guest bytes (`Texture::over_guest`), which take no upload.
    pub textures_in_place: u32,
    /// Nanoseconds of CPU time that the encode took, from the start of `encode_frame` to the submit.
    pub encode_ns: u64,
    /// Nanoseconds that `resolve_handles` took.
    pub resolve_ns: u64,
    /// Nanoseconds that `fit_handles` took.
    pub fit_ns: u64,
    /// Nanoseconds that the barrier plan took.
    pub barrier_ns: u64,
    /// Wall-clock nanoseconds of the chunk encoding, all threads together.
    pub chunks_ns: u64,
    /// Nanoseconds spent making Metal textures.
    pub texture_create_ns: u64,
    /// Nanoseconds spent encoding uploads of guest data into textures.
    pub upload_ns: u64,
    /// Draws that a false render condition left out (not a failure, so not in `skipped`).
    pub render_cond_draws: u32,
    /// Dispatches, copies and downsamples that a false render condition left out.
    pub render_cond_other: u32,
}

impl EncodeStats {
    /// Counts one skipped command.
    pub fn skip(&mut self, why: Skip) {
        self.skipped[why as usize] += 1;
    }

    /// Counts one skipped draw with `args`, also by its class when it is a mesh or indirect draw.
    pub fn skip_draw(&mut self, args: Option<&DrawArgs>, why: Skip) {
        self.skip(why);
        if let Some(c) = args.and_then(DrawClass::of) {
            self.kinds.skipped[c as usize][why as usize] += 1;
        }
    }

    /// All skipped draws.
    pub fn skipped_total(&self) -> u32 {
        self.skipped.iter().sum()
    }

    /// Adds the counters of another task.
    pub fn add(&mut self, o: &Self) {
        self.draws += o.draws;
        for (a, b) in self.skipped.iter_mut().zip(o.skipped) {
            *a += b;
        }
        self.icb_draws += o.icb_draws;
        self.kinds.add(&o.kinds);
        self.icb_executions += o.icb_executions;
        self.icb_builds += o.icb_builds;
        self.pipeline_switches += o.pipeline_switches;
        self.pipeline_builds += o.pipeline_builds;
        self.barriers_emitted += o.barriers_emitted;
        self.barriers_avoided += o.barriers_avoided;
        self.render_passes += o.render_passes;
        self.compute_passes += o.compute_passes;
        self.command_buffers += o.command_buffers;
        self.textures_created += o.textures_created;
        self.textures_rescaled += o.textures_rescaled;
        self.uploads += o.uploads;
        self.copybacks += o.copybacks;
        self.copyback_bytes += o.copyback_bytes;
        self.swizzle_ignored += o.swizzle_ignored;
        self.scaled_upload_lost += o.scaled_upload_lost;
        self.scaled_uploads += o.scaled_uploads;
        self.textures_kept_native += o.textures_kept_native;
        self.textures_in_place += o.textures_in_place;
        self.encode_ns += o.encode_ns;
        self.resolve_ns += o.resolve_ns;
        self.fit_ns += o.fit_ns;
        self.barrier_ns += o.barrier_ns;
        self.chunks_ns += o.chunks_ns;
        self.texture_create_ns += o.texture_create_ns;
        self.upload_ns += o.upload_ns;
        self.render_cond_draws += o.render_cond_draws;
        self.render_cond_other += o.render_cond_other;
    }
}

/// The result of `Gpu::encode_frame`. The `nxkit-nvn` front end owns the resource table, so the caller applies it.
#[derive(Clone, Debug, Default)]
pub struct EncodeReport {
    /// The number of the submitted batch, or 0 when the frame had nothing to submit.
    pub batch: u64,
    /// Textures that the frame deswizzled from guest memory. The caller calls `ResourceTable::synced` for each.
    pub deswizzled: Vec<TexId>,
    /// Textures that the GPU wrote (render targets, copy destinations, dispatch outputs). The caller calls
    /// `ResourceTable::gpu_wrote` for each.
    pub gpu_wrote: Vec<TexId>,
    /// Textures that the frame swizzled back to guest memory for a CPU read. The caller calls
    /// `ResourceTable::synced` for each after `Gpu::wait`.
    pub copied_back: Vec<TexId>,
    /// Textures that the GPU wrote and that the end of the frame copies back into guest memory, because their storage
    /// lies in a memory pool the CPU can access. The copy runs on the GPU after the passes of the frame, so the caller
    /// calls `ResourceTable::synced_at` (after `gpu_wrote_at`) for each without a wait: any later GPU read of the
    /// guest memory, and any CPU read after a fence wait, sees the copy.
    pub mirrored: Vec<TexId>,
    /// The counters.
    pub stats: EncodeStats,
    /// The GPU counters of the frame, which the backend writes when the GPU finished it.
    pub(crate) queries: Option<std::sync::Arc<crate::queries::FrameQueries>>,
}
