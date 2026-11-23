//! Shader functions and their reflection.
//!
//! The shader step of the build writes `<key>_b.metallib` and `<key>_b.json` for every shader (R1, D20). The metallib
//! holds the entry function. The JSON holds the bindings of the host contract. This module reads both, keeps them by
//! key and rejects the programs that the encoder cannot drive. A vertex shader whose JSON has a `pull` field is loaded
//! from its `<key>_bp.metallib` instead while pulling is on (`crate::pull`): the same bindings, an entry that reads its
//! inputs from the layout block at buffer 16. Each stage has one argument buffer at `[[buffer(0)]]`.
//! Member `arg_id` sits at byte `8 * arg_id` and holds a resource ID or a GPU address.

use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};

use nxkit_metal::{Context, ShaderLibrary, VertexFormat};

mod decl;
mod swap;
use decl::{declared_dim, declared_img};

use crate::json::{Json, parse};
use crate::skip::Skip;

/// The entries of `render_scale` in the support buffer: entry 0 is the scale of the pass, entry `1 + i` the scale of
/// descriptor `i`.
pub(crate) const SUPPORT_SCALES: u32 = 73;

/// What a binding of the argument buffer holds. The names are the `kind` strings of `_b.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BindKind {
    /// `cbuf`: the GPU address of uniform buffer `reg`.
    Cbuf,
    /// `tex` and `tbuf`: the resource ID of the texture in slot `reg` (a `tbuf` is a texture buffer).
    Tex,
    /// `samp`: the resource ID of the sampler of the handle in slot `reg`.
    Samp,
    /// `samp_tex`: the same, for a sampler that the combined handle of texture slot `reg` names.
    SampTex,
    /// `tex_pool`: the address of the host table of texture resource IDs.
    TexPool,
    /// `samp_pool`: the address of the host table of sampler resource IDs.
    SampPool,
    /// `drvcb`: the address of the driver constant block.
    DrvCb,
    /// `ubo_ptr`: compute, the GPU address of uniform buffer `reg`.
    UboPtr,
    /// `support`: the translator's support buffer.
    Support,
    /// `border`: the border colour block.
    Border,
    /// `blendz`: the blend word of the draw (`draws::blend_zero_word`), 4 bytes.
    BlendZ,
    /// `sbuf`: the GPU address of storage buffer `reg`.
    Sbuf,
    /// `c1`: the shader's own constant segment, whose bytes the json carries.
    C1,
    /// `img`: the resource ID of the storage image in image slot `reg`.
    Img,
    /// `gmem_map`: the address of the host's memory pool table (`gmem.rs`).
    GmemMap,
    /// `gmem_log`: the address of a zeroed write log for the draw or dispatch (`gmem.rs`).
    GmemLog,
    /// `img_words`: the texels of the texture buffer image in slot `reg` as device words, for its atomics.
    ImgWords,
}

/// How a shader accesses a storage image (`access::` of its `msl_type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImgAccess {
    /// `access::read`.
    Read,
    /// `access::write`.
    Write,
    /// `access::read_write`.
    ReadWrite,
}

/// What a shader declares for an `img` member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ImgDecl {
    /// The access.
    pub(crate) access: ImgAccess,
    /// The shader runs image atomics on it (`atomic` of the reflection).
    pub(crate) atomic: bool,
    /// The texel type is signed integer (`texture2d<int, ...>`). Only matters with `atomic`.
    pub(crate) signed: bool,
    /// A `texture_buffer`: the image is a texture buffer over the guest bytes (`images.rs`).
    pub(crate) buffer: bool,
    /// The texel type is `float` or `half` (not an integer).
    pub(crate) float: bool,
}

impl ImgDecl {
    /// True when the shader may write the image.
    pub(crate) fn writes(&self) -> bool {
        self.atomic || self.access != ImgAccess::Read
    }

    /// The key part of the view the shader needs: 0 for a plain image, 1 for an atomic one on unsigned texels, 2 on
    /// signed.
    pub(crate) fn atomic_code(&self) -> u8 {
        match (self.atomic, self.signed) {
            (false, _) => 0,
            (true, false) => 1,
            (true, true) => 2,
        }
    }
}

/// One member of the argument buffer.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Bind {
    /// The member index. The byte offset is `8 * arg_id`.
    pub(crate) arg_id: u32,
    /// What it holds.
    pub(crate) kind: BindKind,
    /// The slot, from `reg` of the reflection. A null `reg` gives 0.
    pub(crate) reg: u32,
    /// The texture type that the shader declares for a `tex` member, from its `msl_type`.
    pub(crate) dim: Option<nxkit_metal::TextureKind>,
    /// Whether the shader may write an `sbuf` member (`rw` of the reflection). A missing `rw` counts as written.
    pub(crate) rw: bool,
    /// The constant bank of a `drvcb` member: 0 and 2 are the driver block, 24 the push constants.
    pub(crate) bank: u32,
    /// The entry of the support buffer's `render_scale` array this member needs. A `tex` member names the entry of its
    /// texture's scale (`rscale` of the reflection). A `support` member has `Some(0)` when the shader reads the scale
    /// of the pass.
    pub(crate) rscale: Option<u32>,
    /// For an `img` member: what the shader declares.
    pub(crate) img: Option<ImgDecl>,
    /// For a `tex_ubo` or `samp_ubo` member: the words of its handle. It is a `tex` or `samp_tex` member of the texture
    /// slot `reg` that [`crate::ubotex`] gives the source.
    pub(crate) ubo: Option<crate::ubotex::Source>,
}

/// Which mesh pipeline stage a task or mesh shader is, and the `mesh_stage` of the reflection of a task or mesh shader.
pub(crate) use nxkit_pipestate::program::{MeshFacts as MeshInfo, MeshKind};

/// A shader function with its bindings.
pub(crate) struct StageProgram {
    /// The library.
    pub(crate) lib: ShaderLibrary,
    /// The entry function.
    pub(crate) entry: String,
    /// Bit-preserving unsigned and mixed output variants, when the translator supplied them.
    pub(crate) entry_uint: Option<String>,
    pub(crate) entry_mixed: Option<(String, u8)>,
    /// The entry of a vertex shader that writes its layer (`entry_layer`, `[[render_target_array_index]]`).
    pub(crate) entry_layer: Option<String>,
    /// The entries of a vertex or mesh shader that keep the clip depth as it is, for a device in the depth mode 0 to 1
    /// (`entry_z` and `entry_layer_z`). `entry` and `entry_layer` convert the -1 to 1 depth of the GL convention.
    pub(crate) entry_z: [Option<String>; 2],
    /// The entry of a vertex shader that runs only its stores to buffers and images, for a draw with rasterizer
    /// discard on (`entry_discard`). The `_bp` library has none.
    pub(crate) entry_discard: Option<String>,
    /// The members of the argument buffer.
    pub(crate) binds: Vec<Bind>,
    /// The size of the argument buffer in bytes.
    pub(crate) arg_bytes: u32,
    /// The locations of the vertex inputs.
    pub(crate) inputs: Vec<(u32, VertexFormat)>,
    /// The library is the `_bp` library of a vertex shader: it reads its inputs through the `dm_vlayout` block at
    /// `crate::pull::SLOT` and its pipelines have no vertex layout.
    pub(crate) pull: bool,
    /// The colour outputs that a fragment shader writes. `None` means that the reflection does not say.
    pub(crate) colors: Option<Vec<u8>>,
    /// The threadgroup size of a compute shader.
    pub(crate) threadgroup: [u32; 3],
    /// The shader's own constant segment `c[1]`, padded to 16 bytes. Empty when the shader has none.
    pub(crate) c1: Vec<u8>,
    /// The mesh stage of a task or mesh shader. `None` for the other stages.
    pub(crate) mesh: Option<MeshInfo>,
    /// 0 for the shipped library. Else the number of the swap that put a library of source in its place (W37,
    /// `crate::hotswap`). Its pipelines have other keys and are never kept.
    pub(crate) swap: u32,
}

type ProgramMap = crate::fastmap::FastMap<String, Result<Arc<StageProgram>, Skip>>;

/// Reads and caches the programs of one shader directory.
pub(crate) struct Programs {
    ctx: Context,
    dir: PathBuf,
    /// One map per reading of the vertex inputs (index 1 for vertex, 2 for the pulling `_bp` variant of a vertex
    /// shader), so that a lookup by `&str` allocates nothing. A read-write lock: every draw of every encode thread
    /// reads it, and a `Mutex` made them wait for each other.
    cache: RwLock<[ProgramMap; 3]>,
    /// The libraries that a person swapped in (W37). Read on a cache miss only.
    swaps: swap::Swaps,
}

impl Programs {
    pub(crate) fn new(ctx: &Context, dir: PathBuf) -> Self {
        Self {
            ctx: ctx.clone(),
            dir,
            cache: RwLock::new([
                ProgramMap::default(),
                ProgramMap::default(),
                ProgramMap::default(),
            ]),
            swaps: swap::Swaps::default(),
        }
    }

    /// The program of `key`. `vertex` selects the reading of the vertex inputs. The result is cached, also a failure.
    pub(crate) fn stage(&self, key: &str, vertex: bool) -> Result<Arc<StageProgram>, Skip> {
        if let Some(r) =
            self.cache.read().unwrap_or_else(PoisonError::into_inner)[usize::from(vertex)].get(key)
        {
            return r.clone();
        }
        let r = self.load(key, vertex).map(Arc::new);
        if let Err(e) = &r {
            // Counted once per key: the failure is cached.
            let what = format_args!("{key} vertex={vertex}: {e:?}");
            nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::ProgramFailed, what);
            if crate::diag("prog") {
                eprintln!("DIAG prog {what}");
            }
        }
        self.cache.write().unwrap_or_else(PoisonError::into_inner)[usize::from(vertex)]
            .insert(key.to_owned(), r.clone());
        r
    }

    /// The pulling `_bp` variant of the vertex shader `key`, also when pulling is off (`NXKIT_PULL`): a draw whose
    /// attributes the stage_in layout cannot feed takes it (`nxkit_pipestate::nvn::stage_in_fits`). `Skip::Program`
    /// when the shader has no `_bp` library. The result is cached, also a failure.
    pub(crate) fn pulled(&self, key: &str) -> Result<Arc<StageProgram>, Skip> {
        if let Some(p) = self.stage(key, true).ok().filter(|p| p.pull) {
            return Ok(p);
        }
        if let Some(r) = self.cache.read().unwrap_or_else(PoisonError::into_inner)[2].get(key) {
            return r.clone();
        }
        let r = self.load_as(key, true, true, true).map(Arc::new);
        self.cache.write().unwrap_or_else(PoisonError::into_inner)[2]
            .insert(key.to_owned(), r.clone());
        r
    }

    /// Forgets all programs, for example after the shader directory changed.
    pub(crate) fn clear(&self) {
        for m in self
            .cache
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .iter_mut()
        {
            m.clear();
        }
    }

    fn load(&self, key: &str, vertex: bool) -> Result<StageProgram, Skip> {
        self.load_as(key, vertex, vertex && crate::pull::on(), false)
    }

    /// Loads the program of `key`. `pull` asks for the `_bp` library of a vertex shader. Without one the shader stays
    /// on stage_in, unless `require` makes that a failure.
    fn load_as(
        &self,
        key: &str,
        vertex: bool,
        pull: bool,
        require: bool,
    ) -> Result<StageProgram, Skip> {
        if key.is_empty() || key.contains(['/', '\\']) || key.contains("..") {
            return Err(Skip::Program);
        }
        let why = |w: &str| {
            if crate::diag("prog") {
                eprintln!("DIAG prog {key}: {w}");
            }
            Skip::Program
        };
        let text = std::fs::read_to_string(self.dir.join(format!("{key}_b.json")))
            .map_err(|e| why(&format!("json {e}")))?;
        let json = parse(&text).ok_or_else(|| why("json parse"))?;
        let lib = ShaderLibrary::from_path(&self.ctx, &self.dir.join(format!("{key}_b.metallib")))
            .map_err(|e| why(&format!("metallib {e:?}")))?;
        let pull = if vertex && pull {
            self.pull_library(key, &json)
        } else {
            None
        };
        if require && pull.is_none() {
            return Err(why("no _bp library to pull with"));
        }
        if crate::diag("prog")
            && json
                .get("raster")
                .and_then(|r| r.get("primitive_id"))
                .and_then(Json::as_i64)
                .unwrap_or(0)
                != 0
        {
            eprintln!("DIAG prog {key}: primitive_id");
        }
        let mut p = read_stage(&json, lib, vertex)?;
        if let Some((lib, entry)) = pull {
            p.lib = lib;
            p.entry = entry;
            // The `_bp` library has its pulling entry and, from a current translator, the one that keeps the clip
            // depth (`pull.entry_z`). Without it a device in the depth mode 0 to 1 draws with the converting entry
            // (`pipe.depth-mode-ignored`).
            p.entry_z = [nxkit_pipestate::program::pull_entry_z(&json), None];
            p.entry_discard = None;
            p.pull = true;
        }
        self.swaps.apply(key, &mut p);
        Ok(p)
    }

    /// The `_bp` library of a vertex shader and its entry, when the JSON has a `pull` field (the step removes the
    /// field when `_bp` did not compile). A library that does not load leaves the shader on `_b` and stage_in.
    fn pull_library(&self, key: &str, json: &Json) -> Option<(ShaderLibrary, String)> {
        let entry = pull_entry(json)?;
        match ShaderLibrary::from_path(&self.ctx, &self.dir.join(format!("{key}_bp.metallib"))) {
            Ok(lib) => Some((lib, entry)),
            Err(e) => {
                if crate::diag("prog") {
                    eprintln!("DIAG prog {key}: _bp metallib {e:?}, drawn with stage_in");
                }
                None
            }
        }
    }
}

/// The entry of the `_bp` library that a `pull` field names (`nxkit_pipestate::program::pull_entry`). `nxb-archive`
/// `plan` archives exactly this function.
pub(crate) fn pull_entry(json: &Json) -> Option<String> {
    nxkit_pipestate::program::pull_entry(json)
}

fn read_stage(json: &Json, lib: ShaderLibrary, vertex: bool) -> Result<StageProgram, Skip> {
    let facts = nxkit_pipestate::program::read(json, vertex).map_err(|_| Skip::Program)?;
    let mut binds = Vec::new();
    let pass_scale = json
        .get("pass_scale")
        .and_then(Json::as_bool)
        .unwrap_or(false);
    let mut arg_bytes = 0u32;
    // The distinct sources of the `tex_ubo` and `samp_ubo` members. The index gives the texture slot.
    let mut sources: Vec<crate::ubotex::Source> = Vec::new();
    for b in json.get("bindings").map(Json::as_arr).unwrap_or(&[]) {
        let kind_str = b.get("kind").and_then(Json::as_str).ok_or(Skip::Program)?;
        let ubo = match kind_str {
            "tex_ubo" => crate::ubotex::source(b, false),
            "samp_ubo" => crate::ubotex::source(b, true),
            _ => None,
        };
        let kind = match kind_str {
            "tex_ubo" if ubo.is_some() => BindKind::Tex,
            "samp_ubo" if ubo.is_some() => BindKind::SampTex,
            "cbuf" => BindKind::Cbuf,
            // `tbuf` is a texture buffer in a texture slot, which the translator declares as a `texture_buffer`: the
            // `tex` path below binds it as a sampled texture buffer.
            "tex" | "tbuf" => BindKind::Tex,
            "samp" => BindKind::Samp,
            "samp_tex" => BindKind::SampTex,
            "tex_pool" => BindKind::TexPool,
            "samp_pool" => BindKind::SampPool,
            "drvcb" => BindKind::DrvCb,
            "ubo_ptr" => BindKind::UboPtr,
            "support" => BindKind::Support,
            "border" => BindKind::Border,
            "blendz" => BindKind::BlendZ,
            "sbuf" => BindKind::Sbuf,
            "c1" => BindKind::C1,
            "img" => BindKind::Img,
            "gmem_map" => BindKind::GmemMap,
            "gmem_log" => BindKind::GmemLog,
            "img_words" => BindKind::ImgWords,
            other => {
                // The reason names the kind, so the health report says which one is missing.
                nxkit_nvn::diag::example(
                    nxkit_nvn::diag::Reason::SkipBinding,
                    format_args!(
                        "binding kind {other} (arg {})",
                        b.get("arg_id").and_then(Json::as_i64).unwrap_or(-1)
                    ),
                );
                nxkit_nvn::diag::note(
                    nxkit_nvn::diag::Reason::BindingRefused,
                    format_args!("kind {other}"),
                );
                if crate::diag("prog") {
                    eprintln!("DIAG prog binding kind {other}");
                }
                return Err(Skip::Binding);
            }
        };
        let arg_id = b
            .get("arg_id")
            .and_then(Json::as_i64)
            .ok_or(Skip::Program)?;
        let arg_id = u32::try_from(arg_id).map_err(|_| Skip::Program)?;
        let reg = match ubo {
            Some(src) => {
                let i = sources.iter().position(|s| *s == src).unwrap_or_else(|| {
                    sources.push(src);
                    sources.len() - 1
                });
                crate::ubotex::SLOT_BASE + i as u32
            }
            None => {
                let reg = b.get("reg").and_then(Json::as_i64).unwrap_or(0);
                u32::try_from(reg).map_err(|_| Skip::Program)?
            }
        };
        arg_bytes = arg_bytes.max(
            arg_id
                .checked_add(1)
                .ok_or(Skip::Program)?
                .saturating_mul(8),
        );
        let dim = if matches!(kind, BindKind::Tex | BindKind::Img) {
            b.get("msl_type")
                .and_then(Json::as_str)
                .and_then(declared_dim)
        } else {
            None
        };
        let rw = b.get("rw").and_then(Json::as_bool).unwrap_or(true);
        let rscale = match kind {
            BindKind::Tex | BindKind::Img => b
                .get("rscale")
                .and_then(Json::as_i64)
                .and_then(|v| u32::try_from(v).ok())
                .filter(|v| *v < SUPPORT_SCALES),
            BindKind::Support => pass_scale.then_some(0),
            _ => None,
        };
        // An image, or a sampled texture buffer: both bind a Metal texture buffer (`images.rs`).
        let msl = b.get("msl_type").and_then(Json::as_str).unwrap_or("");
        let img = (kind == BindKind::Img
            || (kind == BindKind::Tex && msl.starts_with("texture_buffer")))
        .then(|| {
            let atomic = b.get("atomic").and_then(Json::as_bool).unwrap_or(false);
            declared_img(msl, atomic)
        });
        let bank = b
            .get("bank")
            .and_then(Json::as_i64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        binds.push(Bind {
            arg_id,
            kind,
            reg,
            dim,
            rw,
            rscale,
            img,
            bank,
            ubo,
        });
    }
    let mut threadgroup = [1u32; 3];
    for (i, v) in json
        .get("threadgroup_size")
        .map(Json::as_arr)
        .unwrap_or(&[])
        .iter()
        .take(3)
        .enumerate()
    {
        threadgroup[i] = v
            .as_i64()
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(1)
            .max(1);
    }
    let mut c1 = Vec::new();
    if let Some(hex) = json.get("c1").and_then(Json::as_str) {
        let b = hex.as_bytes();
        for pair in b.chunks(2) {
            let s = std::str::from_utf8(pair).map_err(|_| Skip::Program)?;
            c1.push(u8::from_str_radix(s, 16).map_err(|_| Skip::Program)?);
        }
        c1.resize(c1.len().next_multiple_of(16).max(16), 0);
    }
    Ok(StageProgram {
        lib,
        entry: facts.entry,
        entry_uint: facts.entry_uint,
        entry_mixed: facts.entry_mixed,
        entry_layer: facts.entry_layer,
        entry_z: facts.entry_z,
        entry_discard: facts.entry_discard,
        binds,
        arg_bytes,
        inputs: facts.inputs,
        pull: false,
        colors: facts.colors,
        threadgroup,
        c1,
        mesh: facts.mesh,
        swap: 0,
    })
}

/// Reads the `mesh_stage` object of a task or mesh library.
#[cfg(test)]
mod img_tests {
    use super::*;

    #[test]
    fn an_img_member_gives_its_access_and_texel_type() {
        let d = declared_img("texture2d<float, access::write>", false);
        assert_eq!(
            (d.access, d.atomic, d.signed),
            (ImgAccess::Write, false, false)
        );
        assert!(d.writes());
        let d = declared_img("texture2d_array<float, access::read>", false);
        assert_eq!(d.access, ImgAccess::Read);
        assert!(!d.writes());
        let d = declared_img("texture2d<uint, access::read_write>", true);
        assert_eq!(
            (d.access, d.atomic, d.signed, d.atomic_code()),
            (ImgAccess::ReadWrite, true, false, 1)
        );
        assert_eq!(
            declared_img("texture3d<int, access::read_write>", true).atomic_code(),
            2
        );
    }

    #[test]
    fn the_image_types_of_the_shaders_map_to_texture_kinds() {
        use nxkit_metal::TextureKind as K;
        assert_eq!(declared_dim("texture2d<float, access::write>"), Some(K::D2));
        assert_eq!(
            declared_dim("texture2d_array<float, access::read>"),
            Some(K::D2Array)
        );
        assert_eq!(declared_dim("texture3d<float, access::write>"), Some(K::D3));
    }
}
