//! The encode of a prepared replacement, in the render pass of each replaced draw (on an encode thread).

use super::*;

/// The replacement of one frame.
pub(crate) struct Prepared {
    pub(super) upscale: Upscale,
    pub(super) copies: Vec<PassCopy>,
    pub(super) state: Arc<Mutex<State>>,
}

pub(super) struct Upscale {
    pub(super) draw: u32,
    pub(super) color: Texture,
    pub(super) depth: Texture,
    pub(super) velocity: Texture,
    pub(super) motion: Texture,
    pub(super) output: Texture,
    pub(super) fx: Arc<TemporalUpscaler>,
    pub(super) params: Vec<u8>,
    pub(super) size: [u32; 2],
    pub(super) jitter: (f32, f32),
    pub(super) reset: bool,
    pub(super) depth_reversed: bool,
    pub(super) motion_pipe: ComputePipeline,
    pub(super) copy: RenderPipeline,
    pub(super) format: Format,
}

/// A pass-through draw: `src` is copied into colour attachment 0 of its pass.
pub(super) struct PassCopy {
    pub(super) draw: u32,
    pub(super) src: Texture,
    pub(super) format: Format,
    pub(super) pipe: RenderPipeline,
}

impl Prepared {
    /// Encodes the replacement of draw `first_draw`, the first draw of a render pass, into `cb` before the pass
    /// begins. True when it did: the pass then loads its colour attachment 0 and leaves the draw out. False for a draw
    /// that is not replaced, and for a replacement that failed (logged), whose game draw then runs.
    pub(crate) fn before_pass(
        &self,
        ctx: &Context,
        cb: &mut CommandBuffer,
        scratch: &Scratch,
        first_draw: u32,
        desc: &mut RenderPassDesc,
    ) -> bool {
        let up = self.upscale.draw == first_draw;
        let (src, pipe, format, what) = if up {
            let u = &self.upscale;
            (&u.output, &u.copy, u.format, "upscale")
        } else if let Some(c) = self.copies.iter().find(|c| c.draw == first_draw) {
            (&c.src, &c.pipe, c.format, "pass-through")
        } else {
            return false;
        };
        let fail = |why: String| {
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .fall(why);
            false
        };
        let Some(Some(target)) = desc.colors.first_mut() else {
            return fail(format!("the {what} pass has no colour attachment 0"));
        };
        let (t, s) = (target.texture.desc(), src.desc());
        if target.memoryless || t.format != format || (t.width, t.height) != (s.width, s.height) {
            return fail(format!(
                "colour attachment 0 of the {what} pass is {:?} {}x{}{}, the copy writes {format:?} {}x{}",
                t.format,
                t.width,
                t.height,
                if target.memoryless { " memoryless" } else { "" },
                s.width,
                s.height
            ));
        }
        let encoded = if up {
            self.upscale.encode(ctx, cb, scratch)
        } else {
            Ok(())
        };
        if let Err(e) = encoded.and_then(|()| copy(ctx, cb, src, target, pipe)) {
            return fail(format!("the {what} replacement did not encode: {e}"));
        }
        target.load = LoadAction::Load;
        true
    }
}

impl Upscale {
    /// The motion pass and the scaler.
    fn encode(
        &self,
        ctx: &Context,
        cb: &mut CommandBuffer,
        scratch: &Scratch,
    ) -> Result<(), MetalError> {
        motion_pass(
            ctx,
            cb,
            scratch,
            &self.motion_pipe,
            [&self.depth, &self.velocity, &self.motion],
            &self.params,
            self.size,
        )?;
        let f = TemporalFrame {
            color: &self.color,
            depth: &self.depth,
            motion: &self.motion,
            reactive: None,
            content: (self.size[0], self.size[1]),
            jitter: self.jitter,
            motion_scale: (1.0, 1.0),
            depth_reversed: self.depth_reversed,
            reset: self.reset,
        };
        self.fx.encode(cb, &f, &self.output)
    }
}

/// A compute pass that writes the motion texture (`textures`: depth, velocity, motion) for a picture of `size`.
pub(super) fn motion_pass(
    ctx: &Context,
    cb: &mut CommandBuffer,
    scratch: &Scratch,
    pipe: &ComputePipeline,
    textures: [&Texture; 3],
    params: &[u8],
    size: [u32; 2],
) -> Result<(), MetalError> {
    let addr = scratch.push(params)?;
    let table = ArgumentTable::new(ctx, 1, 3)?;
    let mut cp = cb.begin_compute_pass(&table)?;
    cp.encode(&ComputeCommand::Depend(Dependency {
        scope: Scope::FromEarlierPasses,
        after: Stages::ALL,
        before: Stages::DISPATCH,
        visibility: Visibility::Device,
    }))?;
    cp.encode(&ComputeCommand::SetPipeline(pipe.clone()))?;
    for (i, t) in textures.iter().enumerate() {
        cp.encode(&ComputeCommand::SetTexture {
            index: i as u32,
            texture: (*t).clone(),
        })?;
    }
    cp.encode(&ComputeCommand::SetBuffer {
        index: 0,
        gpu_addr: addr,
    })?;
    cp.encode(&ComputeCommand::DispatchThreads {
        grid: [size[0], size[1], 1],
        threads: [16, 16, 1],
    })?;
    // MetalFX sets no barriers: the scaler's encoders wait behind this one.
    cp.encode(&ComputeCommand::Depend(Dependency {
        scope: Scope::ToLaterPasses,
        after: Stages::DISPATCH,
        before: Stages::ALL,
        visibility: Visibility::Device,
    }))?;
    cp.end();
    cb.retain(textures.map(Texture::clone));
    Ok(())
}

/// A render pass that draws `src` one to one into the texture of `target`.
pub(super) fn copy(
    ctx: &Context,
    cb: &mut CommandBuffer,
    src: &Texture,
    target: &ColorAttachment,
    pipe: &RenderPipeline,
) -> Result<(), MetalError> {
    let table = ArgumentTable::new(ctx, 1, 1)?;
    let desc = RenderPassDesc {
        colors: vec![Some(ColorAttachment {
            depth_plane: target.depth_plane,
            texture: target.texture.clone(),
            load: LoadAction::DontCare,
            store: StoreAction::Store,
            clear: [0.0; 4],
            memoryless: false,
        })],
        depth: None,
        stencil: None,
    };
    let mut rp = cb.begin_render_pass(&desc, &table)?;
    for c in [
        RenderCommand::Depend(Dependency {
            scope: Scope::FromEarlierPasses,
            after: Stages::ALL,
            before: Stages::FRAGMENT,
            visibility: Visibility::Device,
        }),
        RenderCommand::SetPipeline(pipe.clone()),
        RenderCommand::SetTexture {
            index: 0,
            texture: src.clone(),
        },
        RenderCommand::Draw {
            primitive: Primitive::Triangle,
            first_vertex: 0,
            vertex_count: 3,
            instance_count: 1,
            base_instance: 0,
        },
        RenderCommand::Depend(Dependency {
            scope: Scope::ToLaterPasses,
            after: Stages::FRAGMENT,
            before: Stages::ALL,
            visibility: Visibility::Device,
        }),
    ] {
        rp.encode(&c)?;
    }
    rp.end();
    cb.retain((src.clone(), target.texture.clone()));
    Ok(())
}
