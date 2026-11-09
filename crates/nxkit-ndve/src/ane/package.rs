//! The `.mlpackage` directory: `Manifest.json`, the model (`Model.proto` with the MIL program) and the weights.

use super::blob::Entry;
use super::mil::{Dt, OpInfo, value_type};
use super::proto::Msg;

/// Core ML model specification version written (macOS 15).
const SPEC_VERSION: u64 = 9;
/// The MIL program version.
const PROGRAM_VERSION: u64 = 1;
/// The opset of the function (macOS 15).
const OPSET: &str = "CoreML8";
/// `ArrayFeatureType.FLOAT16`.
const ARRAY_F16: u64 = 65552;

/// Path of the manifest in the package.
pub const MANIFEST_PATH: &str = "Manifest.json";
/// Path of the model in the package.
pub const MODEL_PATH: &str = "Data/com.apple.CoreML/model.mlmodel";
/// Path of the weights in the package.
pub const WEIGHTS_PATH: &str = "Data/com.apple.CoreML/weights/weight.bin";

const WEIGHTS_ID: &str = "4F9C989D-144E-421F-A76C-74917CFC8374";
const MODEL_ID: &str = "96188685-17FE-4E2C-9B1E-46D5BCF9CBC6";

/// A model input or output: name and f16 shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TensorSpec {
    /// The feature name.
    pub name: String,
    /// The shape, NCHW.
    pub shape: Vec<usize>,
}

/// A written package.
#[derive(Clone, Debug)]
pub struct Package {
    /// The files, `(path relative to the `.mlpackage` directory, bytes)`, in a fixed order.
    pub files: Vec<(String, Vec<u8>)>,
    /// The model input.
    pub input: TensorSpec,
    /// The model outputs, in the order of the description.
    pub outputs: Vec<TensorSpec>,
    /// Every operation except constants, in program order.
    pub ops: Vec<OpInfo>,
    /// The entries of the weight file.
    pub weights: Vec<Entry>,
}

impl Package {
    /// The bytes of the file at `path`.
    #[must_use]
    pub fn file(&self, path: &str) -> Option<&[u8]> {
        self.files
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, b)| b.as_slice())
    }
}

fn manifest() -> String {
    format!(
        "{{\n    \"fileFormatVersion\": \"1.0.0\",\n    \"itemInfoEntries\": {{\n        \"{WEIGHTS_ID}\": {{\n            \"author\": \"com.apple.CoreML\",\n            \"description\": \"CoreML Model Weights\",\n            \"name\": \"weights\",\n            \"path\": \"com.apple.CoreML/weights\"\n        }},\n        \"{MODEL_ID}\": {{\n            \"author\": \"com.apple.CoreML\",\n            \"description\": \"CoreML Model Specification\",\n            \"name\": \"model.mlmodel\",\n            \"path\": \"com.apple.CoreML/model.mlmodel\"\n        }}\n    }},\n    \"rootModelIdentifier\": \"{MODEL_ID}\"\n}}"
    )
}

fn feature(spec: &TensorSpec) -> Msg {
    let dims: Vec<u64> = spec.shape.iter().map(|&d| d as u64).collect();
    let mut arr = Msg::new();
    arr.packed(1, &dims).uint(2, ARRAY_F16);
    let mut ty = Msg::new();
    ty.msg(5, &arr);
    let mut f = Msg::new();
    f.string(1, &spec.name).msg(3, &ty);
    f
}

/// Assemble the package from the program's operations.
pub(super) fn assemble(
    ops: Vec<Msg>,
    infos: Vec<OpInfo>,
    weights: Vec<u8>,
    entries: Vec<Entry>,
    input: TensorSpec,
    outputs: Vec<TensorSpec>,
) -> Package {
    let mut block = Msg::new();
    for o in &outputs {
        block.string(2, &o.name);
    }
    for op in &ops {
        block.msg(3, op);
    }
    let mut named = Msg::new();
    named
        .string(1, &input.name)
        .msg(2, &value_type(Dt::F16, &input.shape, false));
    let mut func = Msg::new();
    func.msg(1, &named)
        .string(2, OPSET)
        .map_entry(3, OPSET, &block);
    let mut program = Msg::new();
    program.uint(1, PROGRAM_VERSION).map_entry(2, "main", &func);

    let mut desc = Msg::new();
    desc.msg(1, &feature(&input));
    for o in &outputs {
        desc.msg(10, &feature(o));
    }
    let mut model = Msg::new();
    model.uint(1, SPEC_VERSION).msg(2, &desc).msg(502, &program);

    Package {
        files: vec![
            (MANIFEST_PATH.to_owned(), manifest().into_bytes()),
            (MODEL_PATH.to_owned(), model.finish()),
            (WEIGHTS_PATH.to_owned(), weights),
        ],
        input,
        outputs,
        ops: infos,
        weights: entries,
    }
}
