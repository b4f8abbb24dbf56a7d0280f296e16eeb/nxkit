//! Render pipelines made by specializing the build's archived pipelines.
//!
//! The build archives one pipeline per pair of functions, with every colour attachment unspecialized (format, blend
//! and write mask), no vertex layout, no primitive class, one sample, no alpha to coverage and no ICB support
//! (`plan::script_text_with` of `nxb-archive`). An archive answers a lookup only for the state it holds, so a lookup
//! with the formats and blend of a draw misses, and the draw used to compile the functions from scratch. Metal 4 makes
//! the draw's pipeline from the archived one instead: the archive gives the unspecialized pipeline once per pair, and
//! `MTL4Compiler::newRenderPipelineStateBySpecializationWithDescriptor` fills in the colour state, which costs a small
//! fraction of a compile. Only colour attachment state can be specialized, so the archived pipeline serves the draws
//! whose other state (vertex layout, sample count, alpha to coverage, ICB support) is the archived one.
//!
//! With [`ArchiveConfig::compile_bases`](super::ArchiveConfig::compile_bases) a base the archive lacks is compiled
//! once: for a pair the archive lacks, and for every other state of the rest of the pipeline (a vertex layout, four
//! samples, ...), with the colour attachments unspecialized. Every colour state after that is a specialization.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError, RwLock};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTL4Archive, MTL4Compiler, MTL4RenderPipelineDescriptor, MTLPrimitiveTopologyClass,
    MTLRenderPipelineState,
};

use super::PipelineCache;
use super::miss::{assume_defaults, unspecialize_colors};
use crate::pipeline::{RenderPipeline, RenderPipelineDesc};

/// The functions of a pipeline: (vertex library, vertex entry, fragment library, fragment entry). A library is its
/// stem, which the archive knows it by, and the address of its Metal object, so two libraries of one name stay apart.
type Pair = ((String, usize), String, (String, usize), String);

/// A base: the pair, and the hash of the state besides the colour attachments (0 for the state the build archived).
type BaseKey = (Pair, u64);

/// An unspecialized pipeline and the primitive class it was made with, which its specializations must ask for.
#[derive(Clone)]
struct Base {
    pipeline: RenderPipeline,
    topology: MTLPrimitiveTopologyClass,
}

/// The unspecialized pipelines fetched from the archive, and the counters of the specialization.
#[derive(Default)]
pub(super) struct Bases {
    /// By base; `None` when there is no unspecialized pipeline for it, so it is asked for once.
    by_base: RwLock<HashMap<BaseKey, Option<Base>>>,
    found: AtomicU64,
    rejected: AtomicU64,
    compiled: AtomicU64,
    missing: AtomicU64,
    specialized: AtomicU64,
    failed: AtomicU64,
    /// The first error of a specialization, for the diagnostics.
    first_error: Mutex<Option<String>>,
}

/// How the specialization of archived pipelines went so far ([`PipelineCache::base_stats`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BaseStats {
    /// Bases (a pair with the rest of the state) whose unspecialized pipeline the archive gave.
    pub found: u64,
    /// Bases in the archived state whose pair the archive (its shard index) names, but whose lookup missed: the
    /// archive holds other functions or another state than the host asks for.
    pub rejected: u64,
    /// Bases whose unspecialized pipeline the archive lacked and that were compiled.
    pub compiled: u64,
    /// Bases without an unspecialized pipeline: not archived and not compiled, or the compile failed.
    pub missing: u64,
    /// Pipelines made by specialization.
    pub specialized: u64,
    /// Specializations, or compiles of an unspecialized pipeline, that Metal refused; the pipeline was compiled
    /// instead.
    pub failed: u64,
    /// The first refusal.
    pub first_error: Option<String>,
}

/// Whether the state of `desc` other than its colour attachments is the state the build archived, so the archived
/// pipeline of its pair can be specialized to it. The primitive class is left out: the archived pipeline has none,
/// which draws every class (it is what the point size fallback of the host uses).
fn archivable(desc: &RenderPipelineDesc<'_>) -> bool {
    desc.vertex_layout.attributes.is_empty()
        && desc.vertex_layout.layouts.is_empty()
        && desc.sample_count == 1
        && !desc.alpha_to_coverage
        && !desc.support_icb
}

/// The hash of the state of `desc` besides its colour attachments: 0 when it is the archived state, so all such
/// pipelines of a pair share one base whatever their primitive class.
fn rest_of_state(desc: &RenderPipelineDesc<'_>) -> u64 {
    if archivable(desc) {
        return 0;
    }
    let mut h = DefaultHasher::new();
    (
        &desc.vertex_layout,
        desc.topology,
        desc.sample_count,
        desc.alpha_to_coverage,
        desc.support_icb,
    )
        .hash(&mut h);
    h.finish() | 1
}

/// The pair of `desc`, when both libraries came from files that the shard index can name.
fn pair_of(desc: &RenderPipelineDesc<'_>) -> Option<Pair> {
    let (vl, ve) = desc.vertex;
    let (fl, fe) = desc.fragment?;
    let lib = |l: &crate::pipeline::ShaderLibrary| -> Option<(String, usize)> {
        Some((
            l.stem()?.to_string(),
            Retained::as_ptr(&l.raw) as *const () as usize,
        ))
    };
    Some((lib(vl)?, ve.to_string(), lib(fl)?, fe.to_string()))
}

impl PipelineCache {
    /// The pipeline of `desc` specialized from the unspecialized pipeline of its base, from `archive` or compiled.
    /// None for a pipeline without a fragment function (nothing to specialize), when there is no base, or when Metal
    /// refused (counted in [`PipelineCache::base_stats`]).
    pub(super) fn specialize(
        &self,
        archive: Option<&ProtocolObject<dyn MTL4Archive>>,
        desc: &RenderPipelineDesc<'_>,
    ) -> Option<Retained<ProtocolObject<dyn MTLRenderPipelineState>>> {
        if self.exact_only {
            return None;
        }
        desc.fragment?;
        let base = self.base(archive, desc)?;
        let d = self.render_descriptor(desc).ok()?;
        // Specialization keeps what the base has set; ask for the class it has (none for an archived base, none for a
        // vertex function that writes the point size).
        d.setInputPrimitiveTopology(base.topology);
        let b = &self.bases;
        match self
            .generation()
            .compiler
            .newRenderPipelineStateBySpecializationWithDescriptor_pipeline_error(
                &d,
                &base.pipeline.raw,
            ) {
            Ok(raw) => {
                b.specialized.fetch_add(1, Ordering::Relaxed);
                Some(raw)
            }
            Err(e) => {
                b.refused("specialize", desc, &e.localizedDescription().to_string());
                None
            }
        }
    }

    /// The unspecialized pipeline of the base of `desc`, made once per base. A base in the archived state is asked
    /// from the archive; a base the archive lacks is compiled when the cache compiles bases.
    fn base(
        &self,
        archive: Option<&ProtocolObject<dyn MTL4Archive>>,
        desc: &RenderPipelineDesc<'_>,
    ) -> Option<Base> {
        let key = (pair_of(desc)?, rest_of_state(desc));
        let b = &self.bases;
        if let Some(found) = b
            .by_base
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
        {
            return found.clone();
        }
        // Two threads may ask for one base at once and both make it; the second insert counts nothing.
        let d = self.render_descriptor(desc).ok()?;
        let archived = if archivable(desc) {
            assume_defaults(&d);
            let found = archive.and_then(|a| a.newRenderPipelineStateWithDescriptor_error(&d).ok());
            if archive.is_some() && found.is_none() {
                b.rejected.fetch_add(1, Ordering::Relaxed);
            }
            found.map(|raw| (raw, MTLPrimitiveTopologyClass::Unspecified))
        } else {
            unspecialize_colors(&d);
            self.archived_state_base(archive, &d)
        };
        let (found, counter) = match archived {
            Some((raw, topology)) => (
                Some(Base {
                    pipeline: RenderPipeline { raw },
                    topology,
                }),
                &b.found,
            ),
            None if self.compile_bases => match self.compile_base(&d) {
                Ok(base) => (Some(base), &b.compiled),
                Err(e) => {
                    b.refused("base", desc, &e);
                    (None, &b.missing)
                }
            },
            None => (None, &b.missing),
        };
        let fresh = b
            .by_base
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, found.clone())
            .is_none();
        if fresh {
            counter.fetch_add(1, Ordering::Relaxed);
        }
        found
    }

    /// The unspecialized pipeline of a state that the plain base does not have (a vertex layout, several samples, alpha
    /// to coverage, ICB support), from the archive: the build adds the bases of the states a title recorded
    /// (`nxb-archive` `seen`). `d` has its colour attachments unspecialized and the primitive class of the draw. The
    /// class of the draw is asked first, then none, which is what a base that the point size fallback made has. The
    /// answer comes with the class the pipeline was archived with, which its specializations must ask for.
    fn archived_state_base(
        &self,
        archive: Option<&ProtocolObject<dyn MTL4Archive>>,
        d: &MTL4RenderPipelineDescriptor,
    ) -> Option<(
        Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
        MTLPrimitiveTopologyClass,
    )> {
        let a = archive?;
        let topology = d.inputPrimitiveTopology();
        if let Ok(raw) = a.newRenderPipelineStateWithDescriptor_error(d) {
            return Some((raw, topology));
        }
        if topology == MTLPrimitiveTopologyClass::Unspecified {
            return None;
        }
        d.setInputPrimitiveTopology(MTLPrimitiveTopologyClass::Unspecified);
        let found = a.newRenderPipelineStateWithDescriptor_error(d).ok();
        d.setInputPrimitiveTopology(topology);
        found.map(|raw| (raw, MTLPrimitiveTopologyClass::Unspecified))
    }

    /// Compiles the unspecialized pipeline of `d`. Metal refuses a vertex function that writes the point size with a
    /// line or triangle class, as it does for a full compile (the host's `build_render` falls back the same way), so
    /// such a base is compiled again without a class.
    fn compile_base(&self, d: &MTL4RenderPipelineDescriptor) -> Result<Base, String> {
        let compile = |d: &MTL4RenderPipelineDescriptor| {
            self.generation()
                .compiler
                .newRenderPipelineStateWithDescriptor_compilerTaskOptions_error(
                    d,
                    Some(&self.options),
                )
                .map(|raw| RenderPipeline { raw })
                .map_err(|e| e.localizedDescription().to_string())
        };
        let topology = d.inputPrimitiveTopology();
        match compile(d) {
            Ok(pipeline) => Ok(Base { pipeline, topology }),
            Err(e)
                if e.contains("point size")
                    && topology != MTLPrimitiveTopologyClass::Unspecified
                    && topology != MTLPrimitiveTopologyClass::Point =>
            {
                d.setInputPrimitiveTopology(MTLPrimitiveTopologyClass::Unspecified);
                compile(d).map(|pipeline| Base {
                    pipeline,
                    topology: MTLPrimitiveTopologyClass::Unspecified,
                })
            }
            Err(e) => Err(e),
        }
    }

    /// How the specialization of archived pipelines went so far.
    pub fn base_stats(&self) -> BaseStats {
        let b = &self.bases;
        BaseStats {
            found: b.found.load(Ordering::Relaxed),
            rejected: b.rejected.load(Ordering::Relaxed),
            compiled: b.compiled.load(Ordering::Relaxed),
            missing: b.missing.load(Ordering::Relaxed),
            specialized: b.specialized.load(Ordering::Relaxed),
            failed: b.failed.load(Ordering::Relaxed),
            first_error: b
                .first_error
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        }
    }
}

impl Bases {
    /// Counts a refusal of Metal and keeps the first one.
    fn refused(&self, what: &str, desc: &RenderPipelineDesc<'_>, error: &str) {
        self.failed.fetch_add(1, Ordering::Relaxed);
        let mut first = self
            .first_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if first.is_none() {
            *first = Some(format!(
                "{what} {} / {:?}: {error}",
                desc.vertex.1,
                desc.fragment.map(|f| f.1)
            ));
        }
    }
}
