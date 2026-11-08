//! Shader functions and their reflection.
//!
//! The shader step of the build writes `<key>_b.metallib` and `<key>_b.json` for every shader (R1, D20). The metallib
//! holds the entry function. The JSON holds the bindings of the host contract. This module reads both, keeps them by
//! key and rejects the programs that the encoder cannot drive. Each stage has one argument buffer at `[[buffer(0)]]`.
//! Member `arg_id` sits at byte `8 * arg_id` and holds a resource ID or a GPU address.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use nxkit_metal::{Context, ShaderLibrary};

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
}

/// A shader function with its bindings.
pub(crate) struct StageProgram {
    /// The library.
    pub(crate) lib: ShaderLibrary,
    /// The entry function.
    pub(crate) entry: String,
    /// The members of the argument buffer.
    pub(crate) binds: Vec<Bind>,
    /// The size of the argument buffer in bytes.
    pub(crate) arg_bytes: u32,
    /// The locations of the vertex inputs.
    pub(crate) inputs: Vec<u32>,
    /// The colour outputs that a fragment shader writes. `None` means that the reflection does not say.
    pub(crate) colors: Option<Vec<u8>>,
    /// The threadgroup size of a compute shader.
    pub(crate) threadgroup: [u32; 3],
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
    /// One map per reading of the vertex inputs (index 1 for vertex), so that a lookup by `&str` allocates nothing.
    cache: Mutex<[ProgramMap; 2]>,
}

impl Programs {
    pub(crate) fn new(ctx: &Context, dir: PathBuf) -> Self {
        Self {
            ctx: ctx.clone(),
            dir,
            cache: Mutex::new([ProgramMap::default(), ProgramMap::default()]),
        }
    }

    /// The program of `key`. `vertex` selects the reading of the vertex inputs. The result is cached, also a failure.
    pub(crate) fn stage(&self, key: &str, vertex: bool) -> Result<Arc<StageProgram>, Skip> {
        if let Some(r) = self.cache.lock().unwrap_or_else(PoisonError::into_inner)[usize::from(vertex)].get(key) {
            return r.clone();
        }
        let r = self.load(key, vertex).map(Arc::new);
        if let Err(e) = &r {
            if crate::diag("prog") {
                eprintln!("DIAG prog {key} vertex={vertex}: {e:?}");
            }
        }
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)[usize::from(vertex)].insert(key.to_owned(), r.clone());
        r
    }

    /// Forgets all programs, for example after the shader directory changed.
    pub(crate) fn clear(&self) {
        for m in self.cache.lock().unwrap_or_else(PoisonError::into_inner).iter_mut() {
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
    // A vertex shader that pulls its own vertices lives in a `_bp` library with another contract.
    if matches!(json.get("pull"), Some(Json::Obj(_))) {
        return Err(Skip::Program);
    }
    let raster = json.get("raster");
    if raster.and_then(|r| r.get("primitive_id")).and_then(Json::as_i64).unwrap_or(0) != 0 {
        return Err(Skip::Program);
    }
    let entry = json.get("entry").and_then(Json::as_str).unwrap_or("main0").to_owned();
    let mut binds = Vec::new();
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
            _ => return Err(Skip::Binding),
        };
        let arg_id = b.get("arg_id").and_then(Json::as_i64).ok_or(Skip::Program)?;
        let arg_id = u32::try_from(arg_id).map_err(|_| Skip::Program)?;
        let reg = b.get("reg").and_then(Json::as_i64).unwrap_or(0);
        let reg = u32::try_from(reg).map_err(|_| Skip::Program)?;
        arg_bytes = arg_bytes.max(arg_id.checked_add(1).ok_or(Skip::Program)?.saturating_mul(8));
        binds.push(Bind { arg_id, kind, reg });
    }
    let inputs = if vertex {
        json.get("vertex_inputs")
            .map(Json::as_arr)
            .unwrap_or(&[])
            .iter()
            .filter_map(|v| v.get("location").and_then(Json::as_i64))
            .filter_map(|l| u32::try_from(l).ok())
            .collect()
    } else {
        Vec::new()
    };
    let colors = raster.and_then(|r| r.get("colors")).map(|c| {
        c.as_arr().iter().filter_map(Json::as_i64).filter_map(|v| u8::try_from(v).ok()).collect()
    });
    let mut threadgroup = [1u32; 3];
    for (i, v) in json.get("threadgroup_size").map(Json::as_arr).unwrap_or(&[]).iter().take(3).enumerate() {
        threadgroup[i] = v.as_i64().and_then(|v| u32::try_from(v).ok()).unwrap_or(1).max(1);
    }
    Ok(StageProgram { lib, entry, binds, arg_bytes, inputs, colors, threadgroup })
}
