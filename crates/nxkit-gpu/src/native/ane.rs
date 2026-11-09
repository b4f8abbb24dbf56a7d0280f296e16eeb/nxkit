//! The Neural Engine path of a network (`NXKIT_<NAME>_ANE=1`): the Core ML model, its loading on a background thread,
//! and the shared tensors of the slots.
//!
//! The model is written from the weight blobs into a cache directory, compiled and loaded off the render thread, and
//! used only when Core ML puts every operation on the Neural Engine. Until then, and whenever anything fails, the plan
//! of [`super`] runs. A loaded model owns the sets of tensors that [`super::slots`] hands out; [`super::feed`] writes
//! a decoded picture's input into one and runs the model on it, ahead of the game's frame that uses the result.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use nxkit_coreml::{Model, SharedTensor};

use super::iface::{ModelPackage, Offload};
use nxkit_native_plan::GameResource;

/// The sets of tensors. The decoder is three pictures ahead of the game, and two frames of the game are in flight, each
/// reading one result; one more keeps the next picture from taking the slot of a result the game has not read yet.
pub(super) const SLOTS: usize = 6;

/// `NXKIT_<NAME>_ANE_CACHE` names the cache directory; otherwise `~/Library/Caches/nxkit/<name>-ane`.
pub(super) fn cache_root(name: &str, over: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    match (over, home) {
        (Some(o), _) if !o.is_empty() => Some(PathBuf::from(o)),
        (_, Some(h)) if !h.is_empty() => Some(
            Path::new(h)
                .join("Library/Caches/nxkit")
                .join(format!("{name}-ane")),
        ),
        _ => None,
    }
}

/// A directory name for a cache key: letters, digits, `.`, `-` and `_` stay, anything else becomes `_`.
pub(super) fn dir_name(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The directory of the written `.mlpackage` for a cache key.
pub(super) fn package_dir(root: &Path, key: &str) -> PathBuf {
    root.join(format!("{}.mlpackage", dir_name(key)))
}

/// Writes the package files into `dir` through a temporary directory, so a crash leaves no half package. A package
/// that is already there stays.
fn write_package(dir: &Path, files: &[(String, Vec<u8>)]) -> Result<(), String> {
    if dir.is_dir() {
        return Ok(());
    }
    let tmp = dir.with_extension(format!("tmp{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    for (rel, bytes) in files {
        let path = tmp.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    std::fs::rename(&tmp, dir).map_err(|e| format!("rename {}: {e}", tmp.display()))
}

/// The tensors of one slot: the input and the outputs in the order of the offload.
pub(super) struct SlotTensors {
    pub(super) input: SharedTensor,
    pub(super) outputs: Vec<SharedTensor>,
}

/// A model that is loaded, with the tensors of the slots.
pub(super) struct Loaded {
    model: Model,
    names: (String, Vec<String>),
    pub(super) slots: Vec<SlotTensors>,
}

impl Loaded {
    /// Runs the model on the input of `slot` (written) into its outputs, and calls `done` on a Core ML thread when they
    /// are written or the run failed. The slot's tensors must not be touched meanwhile.
    pub(super) fn predict(
        &self,
        slot: usize,
        done: impl FnOnce(Result<(), String>) + Send + 'static,
    ) {
        let t = &self.slots[slot];
        let outs: Vec<(&str, &SharedTensor)> = self
            .names
            .1
            .iter()
            .map(String::as_str)
            .zip(t.outputs.iter())
            .collect();
        self.model
            .predict_async(&[(self.names.0.as_str(), &t.input)], &outs, move |r| {
                done(r.map_err(|e| e.to_string()))
            });
    }
}

/// Where loading is.
pub(super) enum Phase {
    Loading,
    Ready(Arc<Loaded>),
    Failed(String),
}

/// A model that loads in the background. Clones see the same model.
#[derive(Clone)]
pub(super) struct Session {
    phase: Arc<Mutex<Phase>>,
}

impl Session {
    /// Starts loading on a thread. `blobs` are copies of the weight blobs, `key` names the compiled model in the cache.
    pub(super) fn spawn(
        label: &'static str,
        offload: Offload,
        blobs: Vec<Vec<u8>>,
        root: Option<PathBuf>,
        key: String,
    ) -> Self {
        let phase = Arc::new(Mutex::new(Phase::Loading));
        let slot = phase.clone();
        let job = move || {
            let started = Instant::now();
            let r = load(&offload, &blobs, root.as_deref(), &key);
            let mut p = slot.lock().unwrap_or_else(PoisonError::into_inner);
            *p = match r {
                Ok(l) => {
                    eprintln!(
                        "nxkit: {label}.ane: ready after {:.1} s, every operation is on the Neural Engine",
                        started.elapsed().as_secs_f32()
                    );
                    Phase::Ready(Arc::new(l))
                }
                Err(e) => {
                    eprintln!("nxkit: {label}.ane: not used: {e}");
                    Phase::Failed(e)
                }
            };
        };
        if let Err(e) = std::thread::Builder::new()
            .name(format!("{label}-ane"))
            .spawn(job)
        {
            *phase.lock().unwrap_or_else(PoisonError::into_inner) =
                Phase::Failed(format!("no thread: {e}"));
        }
        Self { phase }
    }

    /// The loaded model, or the reason there is none yet (`Err(None)`: still loading).
    pub(super) fn get(&self) -> Result<Arc<Loaded>, Option<String>> {
        match &*self.phase.lock().unwrap_or_else(PoisonError::into_inner) {
            Phase::Ready(l) => Ok(l.clone()),
            Phase::Loading => Err(None),
            Phase::Failed(e) => Err(Some(e.clone())),
        }
    }
}

fn load(
    offload: &Offload,
    blobs: &[Vec<u8>],
    root: Option<&Path>,
    key: &str,
) -> Result<Loaded, String> {
    let root = root.ok_or("no cache directory (no HOME)")?;
    std::fs::create_dir_all(root).map_err(|e| format!("create {}: {e}", root.display()))?;
    let refs: Vec<&[u8]> = blobs.iter().map(Vec::as_slice).collect();
    let pkg: ModelPackage = (offload.package)(&refs)?;
    let dir = package_dir(root, key);
    write_package(&dir, &pkg.files)?;
    let model = Model::compile_and_load(&dir, root, key).map_err(|e| e.to_string())?;
    let plan = model.compute_plan().map_err(|e| e.to_string())?;
    if !plan.all_on_neural_engine() {
        let off: Vec<String> = plan
            .off_neural_engine
            .iter()
            .map(|(op, d)| format!("{op} on {d:?}"))
            .collect();
        return Err(format!(
            "{} of {} operations are not on the Neural Engine: {}",
            plan.total - plan.neural_engine,
            plan.total,
            off.join(", ")
        ));
    }
    let tensor = |name: &str, shape: &[usize]| {
        SharedTensor::new(shape).map_err(|e| format!("tensor {name}: {e}"))
    };
    // The order of the offload's outputs is the order of the slots' tensors.
    let mut out_info: Vec<&(String, Vec<usize>)> = Vec::new();
    for (name, _) in offload.output_tensors {
        out_info.push(
            pkg.outputs
                .iter()
                .find(|o| o.0 == *name)
                .ok_or_else(|| format!("the model has no output {name}"))?,
        );
    }
    if pkg.input.0 != offload.input_tensor {
        return Err(format!(
            "the model's input is {}, not {}",
            pkg.input.0, offload.input_tensor
        ));
    }
    let mut slots = Vec::with_capacity(SLOTS);
    for _ in 0..SLOTS {
        let mut outputs = Vec::new();
        for o in &out_info {
            outputs.push(tensor(&o.0, &o.1)?);
        }
        slots.push(SlotTensors {
            input: tensor(&pkg.input.0, &pkg.input.1)?,
            outputs,
        });
    }
    Ok(Loaded {
        model,
        names: (
            pkg.input.0.clone(),
            offload
                .output_tensors
                .iter()
                .map(|o| o.0.to_owned())
                .collect(),
        ),
        slots,
    })
}

/// The outputs of a slot as the textures the epilogue binds, made once per loaded model.
pub(super) struct SlotTextures {
    pub(super) outputs: Vec<nxkit_metal::Texture>,
}

/// Wraps the outputs of every slot as textures. The input is the host's: nothing on the GPU touches it.
pub(super) fn textures(
    ctx: &nxkit_metal::Context,
    l: &Loaded,
) -> Result<Vec<SlotTextures>, String> {
    let wrap = |t: &SharedTensor| {
        nxkit_metal::Texture::from_io_surface(
            ctx,
            t.io_surface(),
            t.width() as u32,
            t.height() as u32,
            nxkit_metal::Format::R16Float,
            false,
        )
        .map_err(|e| format!("texture of a tensor: {e}"))
    };
    l.slots
        .iter()
        .map(|s| {
            Ok(SlotTextures {
                outputs: s.outputs.iter().map(wrap).collect::<Result<_, String>>()?,
            })
        })
        .collect()
}

/// The textures of a slot's outputs with the resources they are bound as.
pub(super) fn bound(
    outputs: &[(&str, GameResource)],
    t: &SlotTextures,
) -> Vec<(GameResource, nxkit_metal::Texture)> {
    outputs
        .iter()
        .zip(&t.outputs)
        .map(|((_, r), tex)| (*r, tex.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_is_under_the_home_unless_overridden() {
        assert_eq!(
            cache_root("ndve", None, Some("/home/u")),
            Some(PathBuf::from("/home/u/Library/Caches/nxkit/ndve-ane"))
        );
        assert_eq!(
            cache_root("ndve", Some("/x"), Some("/home/u")),
            Some(PathBuf::from("/x"))
        );
        assert_eq!(cache_root("ndve", Some(""), None), None);
    }

    #[test]
    fn a_key_cannot_leave_the_cache() {
        assert_eq!(
            dir_name("ab12-Version 26.0 (Build 25A/1)"),
            "ab12-Version_26.0__Build_25A_1_"
        );
        let d = package_dir(Path::new("/c"), "k/../x");
        assert_eq!(d, PathBuf::from("/c/k_.._x.mlpackage"));
    }

    #[test]
    fn a_package_is_written_once_and_whole() {
        let root = std::env::temp_dir().join(format!("nxkit-ane-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = package_dir(&root, "k");
        let files = vec![
            ("Manifest.json".to_owned(), b"{}".to_vec()),
            (
                "Data/com.apple.CoreML/weights/weight.bin".to_owned(),
                vec![1, 2, 3],
            ),
        ];
        assert!(std::fs::create_dir_all(&root).is_ok());
        assert_eq!(write_package(&dir, &files), Ok(()));
        assert_eq!(
            std::fs::read(dir.join("Data/com.apple.CoreML/weights/weight.bin")).ok(),
            Some(vec![1, 2, 3])
        );
        // A second call leaves it alone, and no temporary directory is left.
        assert_eq!(write_package(&dir, &[]), Ok(()));
        assert_eq!(std::fs::read_dir(&root).map(|d| d.count()).ok(), Some(1));
        let _ = std::fs::remove_dir_all(&root);
    }
}
