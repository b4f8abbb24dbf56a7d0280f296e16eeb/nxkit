//! Shader functions and their reflection.
//!
//! The shader step of the build writes `<key>_b.metallib` and `<key>_b.json` for every shader (R1, D20). The metallib
//! holds the entry function. The JSON holds the bindings of the host contract. This module reads both, keeps them by
//! key and rejects the programs that the encoder cannot drive. Each stage has one argument buffer at `[[buffer(0)]]`.
//! Member `arg_id` sits at byte `8 * arg_id` and holds a resource ID or a GPU address.

use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};

use nxkit_metal::{Context, ShaderLibrary, VertexFormat};

use crate::json::{Json, parse};
use crate::stats::Skip;

/// What a binding of the argument buffer holds. The names are the `kind` strings of `_b.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BindKind {
    /// `cbuf`: the GPU address of uniform buffer `reg`.
    Cbuf,
    /// `tex`: the resource ID of the texture in slot `reg`.
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

/// Reads the access and the texel type from an MSL type such as `texture2d<uint, access::read_write>`.
fn declared_img(msl_type: &str, atomic: bool) -> ImgDecl {
    let args = msl_type.split_once('<').map_or("", |(_, r)| r);
    let access = if args.contains("access::read_write") {
        ImgAccess::ReadWrite
    } else if args.contains("access::write") {
        ImgAccess::Write
    } else {
        ImgAccess::Read
    };
    let signed = args.trim_start().starts_with("int") || args.trim_start().starts_with("short");
    ImgDecl {
        access,
        atomic,
        signed,
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
    /// Whether the shader may write an `sbuf` member (`rw` of the reflection; a missing `rw` counts as written).
    pub(crate) rw: bool,
    /// The constant bank of a `drvcb` member: 0 and 2 are the driver block, 24 the push constants.
    pub(crate) bank: u32,
    /// The entry of the support buffer's `render_scale` array this member needs. A `tex` member names the entry of its
    /// texture's scale (`rscale` of the reflection). A `support` member has `Some(0)` when the shader reads the scale
    /// of the pass.
    pub(crate) rscale: Option<u32>,
    /// For an `img` member: what the shader declares.
    pub(crate) img: Option<ImgDecl>,
}

/// The texture type of an MSL argument type such as `texture2d_array<float>`.
fn declared_dim(msl_type: &str) -> Option<nxkit_metal::TextureKind> {
    use nxkit_metal::TextureKind as K;
    let base = msl_type.split('<').next()?.trim();
    Some(match base {
        "texture2d" | "depth2d" | "texture1d" => K::D2,
        "texture2d_array" | "depth2d_array" | "texture1d_array" => K::D2Array,
        "texturecube" | "depthcube" => K::Cube,
        "texturecube_array" | "depthcube_array" => K::CubeArray,
        "texture3d" => K::D3,
        _ => return None,
    })
}

/// Which mesh pipeline stage a task or mesh shader is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MeshKind {
    /// A task shader, the object function.
    Task,
    /// A mesh shader, the mesh function.
    Mesh,
}

/// The `mesh_stage` of the reflection of a task or mesh shader (`nxb-xlate` `msl::mesh::reflect`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MeshInfo {
    /// The stage.
    pub(crate) kind: MeshKind,
    /// The threads of a workgroup.
    pub(crate) threads: u32,
    /// The bytes of the payload that a task shader writes and its mesh shader reads.
    pub(crate) payload: u32,
    /// For a mesh shader: it reads the payload of a task shader. Without one it reads the first workgroup of the draw.
    pub(crate) task: bool,
}

/// A shader function with its bindings.
pub(crate) struct StageProgram {
    /// The library.
    pub(crate) lib: ShaderLibrary,
    /// The entry function.
    pub(crate) entry: String,
    /// Bit-preserving unsigned and mixed output variants, when the translator supplied them.
    pub(crate) entry_uint: Option<String>,
    pub(crate) entry_mixed: Option<(String, u8)>,
    /// The members of the argument buffer.
    pub(crate) binds: Vec<Bind>,
    /// The size of the argument buffer in bytes.
    pub(crate) arg_bytes: u32,
    /// The locations of the vertex inputs.
    pub(crate) inputs: Vec<(u32, VertexFormat)>,
    /// The colour outputs that a fragment shader writes. `None` means that the reflection does not say.
    pub(crate) colors: Option<Vec<u8>>,
    /// The threadgroup size of a compute shader.
    pub(crate) threadgroup: [u32; 3],
    /// The shader's own constant segment `c[1]`, padded to 16 bytes. Empty when the shader has none.
    pub(crate) c1: Vec<u8>,
    /// The mesh stage of a task or mesh shader. `None` for the other stages.
    pub(crate) mesh: Option<MeshInfo>,
}

impl StageProgram {
    /// True when the stage has a binding of `kind`.
    pub(crate) fn uses_pool(&self, kind: BindKind) -> bool {
        self.binds.iter().any(|b| b.kind == kind)
    }
}

type ProgramMap = crate::draws::FastMap<String, Result<Arc<StageProgram>, Skip>>;

/// Reads and caches the programs of one shader directory.
pub(crate) struct Programs {
    ctx: Context,
    dir: PathBuf,
    /// One map per reading of the vertex inputs (index 1 for vertex), so that a lookup by `&str` allocates nothing. A
    /// read-write lock: every draw of every encode thread reads it, and a `Mutex` made them wait for each other.
    cache: RwLock<[ProgramMap; 2]>,
}

impl Programs {
    pub(crate) fn new(ctx: &Context, dir: PathBuf) -> Self {
        Self {
            ctx: ctx.clone(),
            dir,
            cache: RwLock::new([ProgramMap::default(), ProgramMap::default()]),
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
        if crate::diag("prog") {
            if json
                .get("raster")
                .and_then(|r| r.get("primitive_id"))
                .and_then(Json::as_i64)
                .unwrap_or(0)
                != 0
            {
                eprintln!("DIAG prog {key}: primitive_id");
            }
        }
        read_stage(&json, lib, vertex)
    }
}

fn read_stage(json: &Json, lib: ShaderLibrary, vertex: bool) -> Result<StageProgram, Skip> {
    // A `pull` field names the extra `_bp` library that pulls its own vertices. The `_b` library keeps the stage_in
    // contract, so the field changes nothing here.
    let raster = json.get("raster");
    if raster
        .and_then(|r| r.get("primitive_id"))
        .and_then(Json::as_i64)
        .unwrap_or(0)
        != 0
    {
        return Err(Skip::Program);
    }
    let entry = json
        .get("entry")
        .and_then(Json::as_str)
        .unwrap_or("main0")
        .to_owned();
    let mut binds = Vec::new();
    let pass_scale = json
        .get("pass_scale")
        .and_then(Json::as_bool)
        .unwrap_or(false);
    let mut arg_bytes = 0u32;
    for b in json.get("bindings").map(Json::as_arr).unwrap_or(&[]) {
        let kind = match b.get("kind").and_then(Json::as_str).ok_or(Skip::Program)? {
            "cbuf" => BindKind::Cbuf,
            "tex" => BindKind::Tex,
            "samp" => BindKind::Samp,
            "samp_tex" => BindKind::SampTex,
            "tex_pool" => BindKind::TexPool,
            "samp_pool" => BindKind::SampPool,
            "drvcb" => BindKind::DrvCb,
            "ubo_ptr" => BindKind::UboPtr,
            "support" => BindKind::Support,
            "border" => BindKind::Border,
            "sbuf" => BindKind::Sbuf,
            "c1" => BindKind::C1,
            "img" => BindKind::Img,
            "gmem_map" => BindKind::GmemMap,
            "gmem_log" => BindKind::GmemLog,
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
        let reg = b.get("reg").and_then(Json::as_i64).unwrap_or(0);
        let reg = u32::try_from(reg).map_err(|_| Skip::Program)?;
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
                .filter(|v| *v < crate::bindless::SUPPORT_SCALES),
            BindKind::Support => pass_scale.then_some(0),
            _ => None,
        };
        let img = (kind == BindKind::Img).then(|| {
            let atomic = b.get("atomic").and_then(Json::as_bool).unwrap_or(false);
            declared_img(
                b.get("msl_type").and_then(Json::as_str).unwrap_or(""),
                atomic,
            )
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
        });
    }
    let inputs = if vertex {
        json.get("vertex_inputs")
            .map(Json::as_arr)
            .unwrap_or(&[])
            .iter()
            .filter_map(|v| {
                let location = u32::try_from(v.get("location")?.as_i64()?).ok()?;
                let ty = v.get("msl_type").and_then(Json::as_str).unwrap_or("float4");
                let format = if ty.starts_with("uint") {
                    VertexFormat::UInt4
                } else if ty.starts_with("int") {
                    VertexFormat::Int4
                } else {
                    VertexFormat::Float4
                };
                Some((location, format))
            })
            .collect()
    } else {
        Vec::new()
    };
    let colors = raster.and_then(|r| r.get("colors")).map(|c| {
        c.as_arr()
            .iter()
            .filter_map(Json::as_i64)
            .filter_map(|v| u8::try_from(v).ok())
            .collect()
    });
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
    let mesh = json.get("mesh_stage").map(read_mesh).transpose()?;
    Ok(StageProgram {
        lib,
        entry,
        entry_uint: json
            .get("entry_u")
            .and_then(Json::as_str)
            .map(str::to_owned),
        entry_mixed: json
            .get("entry_m")
            .and_then(Json::as_str)
            .zip(json.get("uint_mask_m").and_then(Json::as_i64))
            .map(|(entry, mask)| (entry.to_owned(), mask as u8)),
        binds,
        arg_bytes,
        inputs,
        colors,
        threadgroup,
        c1,
        mesh,
    })
}

/// Reads the `mesh_stage` object of a task or mesh library.
fn read_mesh(m: &Json) -> Result<MeshInfo, Skip> {
    let kind = match m.get("kind").and_then(Json::as_str) {
        Some("ts") => MeshKind::Task,
        Some("ms") => MeshKind::Mesh,
        _ => return Err(Skip::Program),
    };
    let int = |k: &str| {
        m.get(k)
            .and_then(Json::as_i64)
            .and_then(|v| u32::try_from(v).ok())
    };
    let threads = int("threads")
        .filter(|t| (1..=1024).contains(t))
        .ok_or(Skip::Program)?;
    Ok(MeshInfo {
        kind,
        threads,
        payload: int("payload").unwrap_or(0),
        task: m.get("task").and_then(Json::as_bool).unwrap_or(false),
    })
}

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
