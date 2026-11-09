//! The raw tier of the own front end (first tier, Mac and iOS): a shader goes through `air_front::build_raw` in process,
//! with no Apple tool, when the front end accepts it and the library passes the in-process validation
//! (`nxb_tool::airfront::validate_library`). A reject, an internal error, a panic or a failed validation hands the
//! shader to the next tier (own-identical verdicts, then Apple). The cache-key field is `own-raw@<air-front id>`.
//!
//! Switches: `NXB_OWN_RAW=0` turns only the raw tier off, `NXB_OWN_RAW=list` takes only the shaders of the shipped
//! allow list (`raw-verified.txt`, the old semantics: every entry passed the GPU differential test), `NXB_OWN_RAW=<file>`
//! the shaders of another allow list, `NXB_OWN_FRONT=0` turns every own tier off. Without a value every shader is tried.
//! `NXB_OWN_RAW_IOS=0` turns only the raw tier of the iOS libraries off. No GPU test covers the iOS libraries.

use crate::own_front::{DATA_DIR, front_id};
use std::path::{Path, PathBuf};

/// The raw tier's switch: `0` off, `list` the shipped allow list, a file name another allow list, else every shader.
pub const RAW_SWITCH: &str = "NXB_OWN_RAW";

/// File name of the raw allow list in `<DATA_DIR>/<air-front id>/`.
pub const RAW_LIST: &str = "raw-verified.txt";

/// Which shaders the raw tier tries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Every shader: a reject or a failed validation falls back.
    All,
    /// Only the shaders of this allow list file.
    List(PathBuf),
}

/// The mode of this run from `NXB_OWN_RAW`, and a note for the run report. `None`: the tier is off.
pub fn mode() -> (Option<Mode>, String) {
    let Some(id) = front_id() else {
        return (None, "raw tier off (no air-front feature)".into());
    };
    match std::env::var_os(RAW_SWITCH) {
        Some(v) if v == "0" => (None, format!("raw tier off ({RAW_SWITCH}=0)")),
        Some(v) if v == "list" => {
            let f = Path::new(DATA_DIR).join(id).join(RAW_LIST);
            if f.is_file() {
                let note = format!("raw tier on, allow list {}", f.display());
                (Some(Mode::List(f)), note)
            } else {
                (None, format!("raw tier off ({} missing)", f.display()))
            }
        }
        Some(v) if !v.is_empty() => {
            let note = format!("raw tier on, allow list {}", Path::new(&v).display());
            (Some(Mode::List(PathBuf::from(v))), note)
        }
        _ => (Some(Mode::All), "raw tier on, every shader".to_string()),
    }
}

/// The raw tier of a run, with its counters.
#[cfg(feature = "air-front")]
pub struct RawTier {
    fe: nxb_tool::airfront::RawFrontEnd,
    list: Option<nxb_tool::airfront::RawAllowList>,
    id: String,
    /// Libraries the raw tier made.
    pub libs: std::sync::atomic::AtomicU64,
    /// Shaders the raw tier rejected or failed on (the next tier made them).
    pub fallback: std::sync::atomic::AtomicU64,
}

#[cfg(feature = "air-front")]
impl RawTier {
    /// The tier of this run ([`mode`]), `None` when it is off. A damaged list is an error.
    pub fn open() -> Result<Option<RawTier>, String> {
        let list = match mode().0 {
            None => return Ok(None),
            Some(Mode::All) => None,
            Some(Mode::List(f)) => {
                let text =
                    std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
                Some(nxb_tool::airfront::RawAllowList::parse(&text)?)
            }
        };
        Ok(Some(RawTier {
            fe: nxb_tool::airfront::RawFrontEnd::new(false),
            list,
            id: nxb_tool::airfront::raw_id(),
            libs: Default::default(),
            fallback: Default::default(),
        }))
    }

    /// True when the raw tier tries the shader with this MSL (every shader, or the ones of the allow list).
    pub fn takes(&self, msl: &[u8]) -> bool {
        self.list
            .as_ref()
            .is_none_or(|l| l.allows_digest(&nxb_model::sha256(msl)))
    }

    /// The cache-key field of the raw libraries: `own-raw@<air-front id>`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Number of listed shaders, `None` when every shader is tried.
    pub fn listed(&self) -> Option<usize> {
        self.list
            .as_ref()
            .map(nxb_tool::airfront::RawAllowList::len)
    }

    /// `<n> libraries, <n> fallbacks` for the run note.
    pub fn summary(&self) -> String {
        use std::sync::atomic::Ordering::Relaxed;
        format!(
            "{} libraries, {} fallbacks",
            self.libs.load(Relaxed),
            self.fallback.load(Relaxed)
        )
    }

    /// The raw library of a shader, `None` when the tier does not take it or the build or validation fails.
    pub fn compile(&self, msl: &[u8]) -> Option<(Vec<u8>, nxb_tool::StepCost)> {
        use std::sync::atomic::Ordering::Relaxed;
        if !self.takes(msl) {
            return None;
        }
        match nxb_tool::FrontEnd::compile(&self.fe, msl) {
            Ok(out) => {
                self.libs.fetch_add(1, Relaxed);
                Some((out.metallib, out.steps.first().copied().unwrap_or_default()))
            }
            Err(_) => {
                self.fallback.fetch_add(1, Relaxed);
                None
            }
        }
    }
}

#[cfg(all(test, feature = "air-front"))]
mod tests {
    use super::*;

    #[test]
    fn every_shader_mode_takes_all_and_list_mode_only_listed() {
        let msl = b"fragment float4 f() { return 0; }";
        let mut t = RawTier {
            fe: nxb_tool::airfront::RawFrontEnd::new(false),
            list: None,
            id: nxb_tool::airfront::raw_id(),
            libs: Default::default(),
            fallback: Default::default(),
        };
        assert!(t.takes(msl) && t.takes(b"other") && t.listed().is_none());
        let text = format!("{}\n", nxb_model::sha256(msl).to_hex());
        t.list = Some(nxb_tool::airfront::RawAllowList::parse(&text).expect("list"));
        assert!(t.takes(msl) && !t.takes(b"other") && t.listed() == Some(1));
    }

    #[test]
    fn shipped_list_matches_the_front_end() {
        // The list is bound to the front-end id: a new id needs a new GPU-verified list (no list: tier off).
        let f = Path::new(DATA_DIR)
            .join(front_id().expect("feature on"))
            .join(RAW_LIST);
        if !f.is_file() {
            eprintln!("skipped: no raw list in this tree");
            return;
        }
        let text = std::fs::read_to_string(&f).expect("shipped raw list");
        let list = nxb_tool::airfront::RawAllowList::parse(&text).expect("parses");
        assert!(list.len() > 1000);
    }
}
