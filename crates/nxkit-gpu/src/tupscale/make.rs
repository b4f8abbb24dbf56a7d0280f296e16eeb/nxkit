//! The scaler and the pipelines of the temporal upscale, made once per key and kept in its state.

use super::*;

impl Tupscale {
    /// Makes the scaler for `key` unless the one there fits. True when it is new.
    pub(super) fn scaler(&self, st: &mut State, key: Key) -> Result<bool, Miss> {
        if st.scaler.as_ref().is_some_and(|s| s.key == key) {
            return Ok(false);
        }
        let name = format!(
            "{}x{} -> {}x{} {:?} depth {:?}",
            key.input.0, key.input.1, key.output.0, key.output.1, key.color, key.depth
        );
        if st.refused == Some(key) {
            return Err(why(format!("MetalFX refused a scaler {name}")));
        }
        if st.made_at.is_some_and(|at| st.frame < at + REMAKE_FRAMES) {
            return Err(why(format!(
                "a new scaler {name} waits {REMAKE_FRAMES} frames after the last one"
            )));
        }
        st.scaler = None;
        st.made_at = Some(st.frame);
        st.made += 1;
        let t0 = Instant::now();
        let made = (|| {
            let fx = TemporalUpscaler::new(
                &self.ctx,
                TemporalDesc {
                    input: key.input,
                    output: key.output,
                    color: key.color,
                    depth: key.depth,
                    motion: MOTION_FORMAT,
                    output_format: key.color,
                    reactive: None,
                    hdr: false,
                },
            )?;
            let motion = Texture::new(&self.ctx, own(key.input, MOTION_FORMAT))?;
            let output = Texture::new(&self.ctx, own(key.output, key.color))?;
            Ok::<_, MetalError>(Scaler {
                key,
                fx: Arc::new(fx),
                motion,
                output,
            })
        })();
        match made {
            Ok(s) => {
                eprintln!(
                    "nxkit-gpu: temporal upscale: MetalFX temporal scaler {name} (scaler #{}, made in {:.0} ms)",
                    st.made,
                    t0.elapsed().as_secs_f64() * 1e3
                );
                st.scaler = Some(s);
                Ok(true)
            }
            Err(e) => {
                st.refused = Some(key);
                Err(why(format!("MetalFX refused a scaler {name}: {e}")))
            }
        }
    }

    pub(super) fn library(&self, st: &mut State) -> Result<ShaderLibrary, Miss> {
        if let Some(l) = &st.lib {
            return Ok(l.clone());
        }
        let l = ShaderLibrary::from_source(&self.ctx, msl::LIBRARY, msl::MSL)
            .map_err(|e| why(format!("the shaders do not compile: {e}")))?;
        st.lib = Some(l.clone());
        Ok(l)
    }

    pub(super) fn motion_pipeline(&self, st: &mut State) -> Result<ComputePipeline, Miss> {
        if let Some(p) = &st.motion {
            return Ok(p.clone());
        }
        let lib = self.library(st)?;
        let p = self
            .cache
            .compute(key_of(&(msl::MOTION,)), &lib, msl::MOTION)
            .map_err(|e| why(format!("the motion pipeline: {e}")))?;
        st.motion = Some(p.clone());
        Ok(p)
    }

    pub(super) fn copy_pipeline(
        &self,
        st: &mut State,
        format: Format,
    ) -> Result<RenderPipeline, Miss> {
        if let Some((_, p)) = st.copies.iter().find(|(f, _)| *f == format) {
            return Ok(p.clone());
        }
        let lib = self.library(st)?;
        let desc =
            RenderPipelineDesc::new((&lib, msl::COPY_VS), Some((&lib, msl::COPY_FS)), &[format]);
        let p = self
            .cache
            .render(key_of(&(msl::COPY_FS, format)), &desc)
            .map_err(|e| why(format!("the copy pipeline for {format:?}: {e}")))?;
        st.copies.push((format, p.clone()));
        Ok(p)
    }
}

pub(super) fn key_of(v: &impl Hash) -> u64 {
    let mut h = DefaultHasher::new();
    (msl::LIBRARY, v).hash(&mut h);
    h.finish()
}

/// A private 2D texture of the scaler: the motion it reads or the picture it writes.
pub(super) fn own(size: (u32, u32), format: Format) -> TextureDesc {
    TextureDesc {
        kind: TextureKind::D2,
        width: size.0,
        height: size.1,
        depth: 1,
        mip_levels: 1,
        array_layers: 1,
        allow_views: false,
        format,
        shader_read: true,
        shader_write: true,
        render_target: true,
        storage: Storage::Private,
        shader_atomic: false,
    }
}
