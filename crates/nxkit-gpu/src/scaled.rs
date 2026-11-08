//! Guest data for a texture that the GPU renders at the scaled size (R19).
//!
//! The deswizzle writes the guest layout, which has the native size. So the data goes into a staging texture of the
//! native size first, and a small kernel stretches each level and layer into the scaled texture (`Resampler`). The
//! staging textures come from a pool by description, since a texture that the CPU writes every frame needs one every
//! frame. A pooled texture is used again after `BATCHES_IN_FLIGHT` frames, when the batch ring has waited for the
//! batch that last read it.
//!
//! A pass with all the scalings of the frame follows the last upload in the first command buffer. The scalings write
//! the scaled textures with compute stages, as the plain upload does, so the barrier planner treats both alike.

use nxkit_metal::{
    BATCHES_IN_FLIGHT, CommandBuffer, ComputeCommand, Context, PipelineCache, Resampler, Texture, TextureDesc,
};

use crate::draws::FastMap;
use crate::stats::Skip;
use crate::textures::{Res, Use, describe};

/// Pool entries that no frame used for this many frames are dropped.
const KEEP_FRAMES: u64 = 240;

/// A staging texture and the frame that handed it out last.
struct Entry {
    tex: Texture,
    used: u64,
}

/// A scaling that the frame still has to encode.
struct Pending {
    staging: Texture,
    target: Texture,
    levels: u32,
    slices: u32,
}

/// The staging pool and the scalings of the frame.
pub(crate) struct ScaledUploads {
    ctx: Context,
    resampler: Resampler,
    pool: FastMap<TextureDesc, Vec<Entry>>,
    pending: Vec<Pending>,
    frame: u64,
}

impl ScaledUploads {
    pub(crate) fn new(ctx: &Context, cache: &PipelineCache) -> nxkit_metal::Result<Self> {
        Ok(Self {
            ctx: ctx.clone(),
            resampler: Resampler::new(ctx, cache)?,
            pool: FastMap::default(),
            pending: Vec::new(),
            frame: 0,
        })
    }

    /// Starts a frame: ages the pool.
    pub(crate) fn begin_frame(&mut self) {
        self.frame += 1;
        let frame = self.frame;
        self.pending.clear();
        self.pool.retain(|_, list| {
            list.retain(|e| e.used + KEEP_FRAMES > frame);
            !list.is_empty()
        });
    }

    /// True when the scaled texture `res` can take a staged upload.
    pub(crate) fn supports(res: &Res) -> bool {
        Resampler::supports(&res.desc)
    }

    /// A staging texture for the native-size copy of `rec`'s texture: the one in the pool whose last batch is done, or
    /// a new one. The returned `Res` describes it like an unscaled texture, so the plain upload can fill it.
    pub(crate) fn staging(
        &mut self,
        rec: &nxkit_nvn::resource::TextureRecord,
        res: &Res,
    ) -> Result<Res, Skip> {
        let (desc, widened) = describe(rec, 1.0, Use::default())?;
        let free = BATCHES_IN_FLIGHT as u64;
        let list = self.pool.entry(desc).or_default();
        let tex = match list.iter_mut().find(|e| e.used + free <= self.frame) {
            Some(e) => {
                e.used = self.frame;
                e.tex.clone()
            }
            None => {
                let tex = Texture::new(&self.ctx, desc).map_err(|e| {
                    let what = format_args!("staging {desc:?}: {e:?}");
                    nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::TextureCreate, what);
                    crate::tex_skip(what)
                })?;
                list.push(Entry { tex: tex.clone(), used: self.frame });
                tex
            }
        };
        Ok(Res {
            barrier_id: res.barrier_id,
            tex,
            desc,
            generation: 0,
            scaled: false,
            fresh: true,
            widened,
        })
    }

    /// Queues the scaling of the filled staging texture into the scaled texture of `res`.
    pub(crate) fn queue(&mut self, staging: &Res, res: &Res) {
        self.pending.push(Pending {
            staging: staging.tex.clone(),
            target: res.tex.clone(),
            levels: staging.desc.mip_levels.min(res.desc.mip_levels),
            slices: staging.desc.slice_count().min(res.desc.slice_count()),
        });
    }

    /// Encodes the queued scalings into one compute pass of `cb`. Returns how many textures it scaled and how many it
    /// lost. The pass waits for the earlier passes of the queue, which hold the uploads of the staging textures.
    pub(crate) fn flush(&mut self, cb: &mut CommandBuffer) -> (u32, u32) {
        if self.pending.is_empty() {
            return (0, 0);
        }
        let pending = std::mem::take(&mut self.pending);
        let all = pending.len() as u32;
        let Ok(mut pass) = cb.begin_compute_pass(self.resampler.table()) else {
            return (0, all);
        };
        if pass.encode(&ComputeCommand::WaitForEarlierPasses).is_err() {
            pass.end();
            return (0, all);
        }
        let mut lost = 0;
        for p in &pending {
            let mut ok = true;
            for slice in 0..p.slices {
                for level in 0..p.levels {
                    if let Err(e) = self.resampler.encode(&mut pass, &p.staging, &p.target, level, slice) {
                        nxkit_nvn::diag::note(
                            nxkit_nvn::diag::Reason::UploadFailed,
                            format_args!("scale {:?} level {level} slice {slice}: {e}", p.target.desc().format),
                        );
                        ok = false;
                    }
                }
            }
            lost += u32::from(!ok);
        }
        pass.retain(pending);
        pass.end();
        (all - lost, lost)
    }
}
