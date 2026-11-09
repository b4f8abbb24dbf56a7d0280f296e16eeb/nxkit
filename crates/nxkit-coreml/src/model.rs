//! `Model`: compile, cache and load a Core ML package.

use std::path::{Path, PathBuf};

use objc2::rc::{Retained, autoreleasepool};
use objc2_core_ml::{MLComputeUnits, MLModel, MLModelConfiguration, MLMultiArrayConstraint};
use objc2_foundation::{NSString, NSURL};

use crate::cache;
use crate::error::{CoreMlError, Result, ns_text};

/// The name and shape of one input or output of a model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TensorInfo {
    /// The feature name in the model.
    pub name: String,
    /// The shape the model wants, outermost dimension first.
    pub shape: Vec<usize>,
}

/// A loaded Core ML model that runs on the CPU and the Neural Engine, never on the GPU.
pub struct Model {
    pub(crate) model: Retained<MLModel>,
    pub(crate) config: Retained<MLModelConfiguration>,
    pub(crate) compiled: PathBuf,
    inputs: Vec<TensorInfo>,
    outputs: Vec<TensorInfo>,
}

// SAFETY: Core ML documents `MLModel` prediction as thread safe, and the configuration is not changed after loading.
unsafe impl Send for Model {}
// SAFETY: see `Send`.
unsafe impl Sync for Model {}

fn file_url(p: &Path, dir: bool) -> Retained<NSURL> {
    NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&p.to_string_lossy()), dir)
}

fn shape_of(c: Option<Retained<MLMultiArrayConstraint>>) -> Vec<usize> {
    c.map(|c| {
        // SAFETY: `shape` is a plain getter of an immutable object.
        unsafe { c.shape() }
            .iter()
            .map(|n| n.integerValue().max(0) as usize)
            .collect()
    })
    .unwrap_or_default()
}

impl Model {
    /// Compiles `mlpackage_dir` with Core ML, unless the cache directory already has a compiled model under
    /// `cache_key`, and loads it for the CPU and the Neural Engine.
    ///
    /// The compile takes seconds and the first load on the Neural Engine more: call this on a background thread. The
    /// key must change whenever the package or macOS changes, so build it from a hash of the package and
    /// [`os_build_tag`](crate::os_build_tag). Characters other than letters, digits, `.`, `-` and `_` are replaced in
    /// the directory name.
    pub fn compile_and_load(
        mlpackage_dir: &Path,
        cache_dir: &Path,
        cache_key: &str,
    ) -> Result<Self> {
        autoreleasepool(|_| {
            let entry = cache::entry_path(cache_dir, cache_key)?;
            if !entry.is_dir() {
                if !mlpackage_dir.exists() {
                    return Err(CoreMlError::Io(format!(
                        "{} does not exist",
                        mlpackage_dir.display()
                    )));
                }
                let url = file_url(mlpackage_dir, true);
                // SAFETY: a plain class method. The asynchronous variant gives the same result through a block; this
                // call is already on a background thread, so the blocking one is the simpler.
                #[allow(deprecated)]
                let compiled = unsafe { MLModel::compileModelAtURL_error(&url) }
                    .map_err(|e| CoreMlError::Compile(ns_text(&e)))?;
                let tmp = compiled
                    .path()
                    .map(|p| PathBuf::from(p.to_string()))
                    .ok_or_else(|| CoreMlError::Compile("the compiled URL has no path".into()))?;
                let stored = cache::store(&tmp, &entry);
                let _ = std::fs::remove_dir_all(&tmp);
                stored?;
            }
            Self::load(entry)
        })
    }

    fn load(compiled: PathBuf) -> Result<Self> {
        // SAFETY: plain constructor and setter on an object nobody else holds.
        let config = unsafe {
            let c = MLModelConfiguration::new();
            c.setComputeUnits(MLComputeUnits::CPUAndNeuralEngine);
            c
        };
        let url = file_url(&compiled, true);
        // SAFETY: `url` points at a compiled model directory.
        let model = unsafe { MLModel::modelWithContentsOfURL_configuration_error(&url, &config) }
            .map_err(|e| CoreMlError::Load(ns_text(&e)))?;
        // SAFETY: getters of immutable description objects.
        let (inputs, outputs) = unsafe {
            let d = model.modelDescription();
            let list = |m: Retained<
                objc2_foundation::NSDictionary<NSString, objc2_core_ml::MLFeatureDescription>,
            >| {
                let (keys, values) = m.to_vecs();
                let mut v: Vec<TensorInfo> = keys
                    .iter()
                    .zip(values.iter())
                    .map(|(k, f)| TensorInfo {
                        name: k.to_string(),
                        shape: shape_of(f.multiArrayConstraint()),
                    })
                    .collect();
                v.sort_by(|a, b| a.name.cmp(&b.name));
                v
            };
            (
                list(d.inputDescriptionsByName()),
                list(d.outputDescriptionsByName()),
            )
        };
        Ok(Self {
            model,
            config,
            compiled,
            inputs,
            outputs,
        })
    }

    /// The inputs of the model, sorted by name, with the shapes it wants.
    pub fn inputs(&self) -> &[TensorInfo] {
        &self.inputs
    }

    /// The outputs of the model, sorted by name, with their shapes.
    pub fn outputs(&self) -> &[TensorInfo] {
        &self.outputs
    }

    /// The directory of the compiled model in the cache.
    pub fn compiled_path(&self) -> &Path {
        &self.compiled
    }
}
