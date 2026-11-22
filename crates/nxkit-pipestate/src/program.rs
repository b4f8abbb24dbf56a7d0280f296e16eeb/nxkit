//! What a pipeline needs to know of a translated shader: the facts of its `<key>_b.json`.
//!
//! The host reads them when it loads a program (`nxkit-gpu` `programs.rs`), and the build reads them when it resolves a
//! seed (`crate::seeds`). Both read through [`read`], so a field that the pipeline depends on is read the same way. The
//! JSON parser is the caller's: [`JsonView`] is the little this module needs of it.

use crate::state::VertexFormat;

/// The vertex buffer slot of the layout block of a vertex function that pulls (`dm_vlayout`).
pub const PULL_SLOT: u32 = 16;

/// The entry names the translator gives the variants of a vertex function (`nxb-xlate` `variant/vertex.rs`): the
/// entry that keeps the clip depth, the layer entry and the layer entry that keeps the clip depth.
pub const ENTRY_Z: &str = "main0_z";
/// The layer entry of a vertex function.
pub const ENTRY_LAYER: &str = "main0_l";
/// The layer entry that keeps the clip depth.
pub const ENTRY_LAYER_Z: &str = "main0_lz";

/// A read-only view of a JSON value.
pub trait JsonView<'a>: Copy {
    /// The member `key` of an object.
    fn get(self, key: &str) -> Option<Self>;
    /// The text of a string.
    fn as_str(self) -> Option<&'a str>;
    /// The value of an integer.
    fn as_i64(self) -> Option<i64>;
    /// The value of a boolean.
    fn as_bool(self) -> Option<bool>;
    /// The items of an array, empty for any other value.
    fn items(self) -> Vec<Self>;
}

/// Which mesh pipeline stage a task or mesh shader is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeshKind {
    /// A task shader, the object function.
    Task,
    /// A mesh shader, the mesh function.
    Mesh,
}

/// The `mesh_stage` of a task or mesh shader (`nxb-xlate` `msl::mesh::reflect`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshFacts {
    /// The stage.
    pub kind: MeshKind,
    /// The threads of a workgroup.
    pub threads: u32,
    /// The bytes of the payload that a task shader writes and its mesh shader reads.
    pub payload: u32,
    /// For a mesh shader: it reads the payload of a task shader. Without one it reads the first workgroup of the draw.
    pub task: bool,
}

/// Why a program has no facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactsError {
    /// The fragment shader reads the primitive ID, which the host does not draw.
    PrimitiveId,
    /// The `mesh_stage` field does not parse.
    Mesh,
}

/// The facts of one translated shader that its pipelines depend on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgramFacts {
    /// The entry function.
    pub entry: String,
    /// The bit-preserving unsigned output variant of a fragment shader (`entry_u`).
    pub entry_uint: Option<String>,
    /// The mixed output variant of a fragment shader and the mask of its unsigned outputs (`entry_m`, `uint_mask_m`).
    pub entry_mixed: Option<(String, u8)>,
    /// The entry of a vertex shader that writes its layer (`entry_layer`).
    pub entry_layer: Option<String>,
    /// The entries that keep the clip depth (`entry_z`, `entry_layer_z`).
    pub entry_z: [Option<String>; 2],
    /// The vertex inputs: location and the type the shader reads (`Float4`, `UInt4` or `Int4`). Empty unless read as a
    /// vertex shader.
    pub inputs: Vec<(u32, VertexFormat)>,
    /// The colour outputs that a fragment shader writes. `None` means that the reflection does not say.
    pub colors: Option<Vec<u8>>,
    /// The entry of the `_bp` library of a vertex shader that pulls its inputs, when the `pull` field is at
    /// [`PULL_SLOT`].
    pub pull: Option<String>,
    /// The mesh stage of a task or mesh shader.
    pub mesh: Option<MeshFacts>,
}

/// The type a vertex input is read as, from its `msl_type`.
fn input_type(ty: &str) -> VertexFormat {
    if ty.starts_with("uint") {
        VertexFormat::UInt4
    } else if ty.starts_with("int") {
        VertexFormat::Int4
    } else {
        VertexFormat::Float4
    }
}

fn read_mesh<'a, J: JsonView<'a>>(m: J) -> Result<MeshFacts, FactsError> {
    let kind = match m.get("kind").and_then(J::as_str) {
        Some("ts") => MeshKind::Task,
        Some("ms") => MeshKind::Mesh,
        _ => return Err(FactsError::Mesh),
    };
    let int = |k: &str| {
        m.get(k)
            .and_then(J::as_i64)
            .and_then(|v| u32::try_from(v).ok())
    };
    let threads = int("threads")
        .filter(|t| (1..=1024).contains(t))
        .ok_or(FactsError::Mesh)?;
    Ok(MeshFacts {
        kind,
        threads,
        payload: int("payload").unwrap_or(0),
        task: m.get("task").and_then(J::as_bool).unwrap_or(false),
    })
}

/// The entry of the `_bp` library that a `pull` field names (`main0` when the field has none), or `None` without a
/// field or for a field at another slot than [`PULL_SLOT`].
pub fn pull_entry<'a, J: JsonView<'a>>(json: J) -> Option<String> {
    let p = json.get("pull")?;
    if p.get("slot").and_then(J::as_i64) != Some(i64::from(PULL_SLOT)) {
        return None;
    }
    Some(
        p.get("entry")
            .and_then(J::as_str)
            .filter(|e| !e.is_empty())
            .unwrap_or("main0")
            .to_owned(),
    )
}

/// Reads the facts of a `<key>_b.json`. `vertex` reads the vertex inputs and the `pull` field.
pub fn read<'a, J: JsonView<'a>>(json: J, vertex: bool) -> Result<ProgramFacts, FactsError> {
    let raster = json.get("raster");
    if raster
        .and_then(|r| r.get("primitive_id"))
        .and_then(J::as_i64)
        .unwrap_or(0)
        != 0
    {
        return Err(FactsError::PrimitiveId);
    }
    let text = |k: &str| json.get(k).and_then(J::as_str).map(str::to_owned);
    let inputs = if vertex {
        json.get("vertex_inputs")
            .map(J::items)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| {
                let location = u32::try_from(v.get("location")?.as_i64()?).ok()?;
                let ty = v.get("msl_type").and_then(J::as_str).unwrap_or("float4");
                Some((location, input_type(ty)))
            })
            .collect()
    } else {
        Vec::new()
    };
    let colors = raster.and_then(|r| r.get("colors")).map(|c| {
        c.items()
            .into_iter()
            .filter_map(J::as_i64)
            .filter_map(|v| u8::try_from(v).ok())
            .collect()
    });
    Ok(ProgramFacts {
        entry: text("entry").unwrap_or_else(|| "main0".to_owned()),
        entry_uint: text("entry_u"),
        entry_mixed: text("entry_m")
            .zip(json.get("uint_mask_m").and_then(J::as_i64))
            .map(|(entry, mask)| (entry, mask as u8)),
        entry_layer: text("entry_layer"),
        entry_z: [text("entry_z"), text("entry_layer_z")],
        inputs,
        colors,
        pull: if vertex { pull_entry(json) } else { None },
        mesh: json.get("mesh_stage").map(read_mesh).transpose()?,
    })
}

/// The library stem of a shader key: `<key>_b`, or `<key>_bp` for the library of a vertex function that pulls.
pub fn library_stem(key: &str, pull: bool) -> String {
    if pull {
        format!("{key}_bp")
    } else {
        format!("{key}_b")
    }
}

/// The shader key of a library stem (`<key>_b` or `<key>_bp`), and whether it is a pulling library.
pub fn key_of_stem(stem: &str) -> (&str, bool) {
    if let Some(k) = stem.strip_suffix("_bp") {
        (k, true)
    } else {
        (stem.strip_suffix("_b").unwrap_or(stem), false)
    }
}
