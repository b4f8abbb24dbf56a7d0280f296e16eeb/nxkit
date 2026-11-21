//! The shaders of the game: the Metal library `vk_shaders.metallib` and its table `vk_shaders.tsv`, loaded once
//! (D21, the reference layer's `sh_load` and `sh_find`).
//!
//! A shader module is found by its key ([`ShaderKey`], the head of the SHA-256 of its SPIR-V): the row gives the stage
//! and the Metal slot of each descriptor, the library the function `vk_<key>` (and `vk_<key>_d`, the dual-source
//! variant of a fragment shader).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use nxkit_abi::vk_table::{self, Row, Stage};
use nxkit_metal::{Context, ShaderLibrary};
use nxkit_vk::desc::ShaderKey;

/// One shader of the table, ready for a pipeline.
#[derive(Clone, Debug)]
pub(crate) struct Shader {
    /// The row of the table.
    pub(crate) row: Arc<Row>,
    /// The Metal function of the shader, `vk_<key>`.
    pub(crate) function: String,
    /// The dual-source variant, `vk_<key>_d`, when the library has it.
    pub(crate) dual: Option<String>,
}

/// The library and the table of the game's shaders.
pub struct ShaderTable {
    library: Option<ShaderLibrary>,
    rows: HashMap<ShaderKey, Arc<Row>>,
    functions: HashSet<String>,
    /// Why the table or the library is missing, for the messages of the pipelines that cannot be made.
    problem: Option<String>,
}

impl ShaderTable {
    /// The table over `rows` with the functions of `library`. A test makes one from MSL source.
    pub fn new(library: Option<ShaderLibrary>, rows: Vec<Row>) -> Self {
        let functions = library
            .as_ref()
            .map(|l| l.function_names().into_iter().collect())
            .unwrap_or_default();
        Self {
            library,
            rows: rows
                .into_iter()
                .map(|r| (ShaderKey(r.key), Arc::new(r)))
                .collect(),
            functions,
            problem: None,
        }
    }

    /// Loads `dir/vk_shaders.tsv` and `dir/vk_shaders.metallib`. A file that is missing or does not load gives an empty
    /// table that names the problem: every pipeline then fails with it, and the game runs on without pictures.
    pub fn load(ctx: &Context, dir: &Path) -> Self {
        let tsv = dir.join(vk_table::FILE);
        let lib = dir.join(vk_table::LIBRARY);
        let rows = std::fs::read_to_string(&tsv)
            .map_err(|e| format!("cannot read {}: {e}", tsv.display()))
            .and_then(|t| vk_table::parse(&t).map_err(|e| format!("{}: {e}", tsv.display())));
        let library = ShaderLibrary::from_path(ctx, &lib)
            .map_err(|e| format!("cannot load {}: {e}", lib.display()));
        let problem = rows.as_ref().err().or(library.as_ref().err()).cloned();
        let mut t = Self::new(library.ok(), rows.unwrap_or_default());
        match &problem {
            Some(p) => eprintln!("[vk] [RED] the shader table: {p}"),
            None => eprintln!(
                "[vk] shader table {}: {} shaders, {} functions",
                dir.display(),
                t.rows.len(),
                t.functions.len()
            ),
        }
        t.problem = problem;
        t
    }

    /// The library, None when it did not load.
    pub(crate) fn library(&self) -> Option<&ShaderLibrary> {
        self.library.as_ref()
    }

    /// Whether the table has a row for `key`.
    pub(crate) fn has(&self, key: ShaderKey) -> bool {
        self.rows.contains_key(&key)
    }

    /// The shader of `key` for a stage: its row, which must be of that stage, and its function, which the library must
    /// have. The error names the key.
    pub(crate) fn shader(&self, key: ShaderKey, want: StageKind) -> Result<Shader, String> {
        let hex = key.to_hex();
        let row = self.rows.get(&key).ok_or_else(|| match &self.problem {
            Some(p) => format!("shader {hex} has no table ({p})"),
            None => format!("shader {hex} is not in the shader table"),
        })?;
        if StageKind::of(row.stage) != want {
            return Err(format!(
                "shader {hex} is a {} shader in the table, the pipeline uses it as {want:?}",
                row.stage
            ));
        }
        let function = vk_table::function(&key.0);
        if !self.functions.contains(&function) {
            return Err(format!(
                "shader {hex}: the library has no function {function}"
            ));
        }
        let dual = vk_table::dual_function(&key.0);
        Ok(Shader {
            row: row.clone(),
            dual: self.functions.contains(&dual).then_some(dual),
            function,
        })
    }
}

/// The stage of a shader, without the threadgroup size of a kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StageKind {
    /// A vertex shader.
    Vertex,
    /// A fragment shader.
    Fragment,
    /// A compute shader.
    Compute,
}

impl StageKind {
    fn of(s: Stage) -> Self {
        match s {
            Stage::Vert => Self::Vertex,
            Stage::Frag => Self::Fragment,
            Stage::Comp(_) => Self::Compute,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TSV: &str = "# test\n0fcca318097ae0a2\tvert\tu:1.47=0\t/a.vert\n2e8ca24d37836637\tfrag\ts:0.24=0 t:1.25=0\t/b.frag\n";

    #[test]
    fn rows_are_found_by_key_and_stage() {
        let rows = vk_table::parse(TSV).unwrap_or_default();
        let t = ShaderTable::new(None, rows);
        let k = ShaderKey(vk_table::parse_key("0fcca318097ae0a2").unwrap_or_default());
        assert!(t.has(k));
        // No library: the function is missing, and the message names it.
        let e = t.shader(k, StageKind::Vertex).err().unwrap_or_default();
        assert!(e.contains("vk_0fcca318097ae0a2"), "{e}");
        let e = t.shader(k, StageKind::Fragment).err().unwrap_or_default();
        assert!(e.contains("vert shader"), "{e}");
        let missing = ShaderKey([1; 8]);
        let e = t
            .shader(missing, StageKind::Vertex)
            .err()
            .unwrap_or_default();
        assert!(
            e.contains("0101010101010101") && e.contains("not in"),
            "{e}"
        );
    }
}
