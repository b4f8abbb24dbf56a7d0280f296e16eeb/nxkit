//! The state shared by every task of [`crate::build`] and its output helpers: libraries through single-flight and the
//! store, outputs cloned from the store.

use crate::build::Span;
use crate::build_finish::Gate;
use crate::inputs::ShaderInputs;
use crate::xlate_cache::Xlate;
use nxb_model::{ContentKey, FileDigest};
use nxb_pyjson::{Value, dumps_indent};
use nxb_sched::SingleFlight;
use nxb_shader::side;
use nxb_shader::target::TargetConv;
use nxb_store::Store;
use nxb_tool::{Io, MetalTools};
use nxb_xlate::msl::StageInputs;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

/// One source's library.
#[derive(Clone, Debug)]
pub(crate) struct Libs {
    pub(crate) mac: FileDigest,
}

/// State shared by every task of a build.
pub(crate) struct Shared {
    pub(crate) conv: TargetConv,
    pub(crate) out: PathBuf,
    pub(crate) chain: String,
    pub(crate) ident: String,
    pub(crate) compiler_id: String,
    pub(crate) tools: Arc<MetalTools>,
    pub(crate) own: Option<crate::own_front::OwnFront>,
    pub(crate) pch: Option<PathBuf>,
    pub(crate) io: Io,
    pub(crate) store: Store,
    pub(crate) xlate: Xlate,
    /// The inputs of the pixel shaders finished so far, for their vertex shaders.
    pub(crate) inputs_of: Mutex<BTreeMap<String, StageInputs>>,
    pub(crate) inputs: ShaderInputs,
    pub(crate) pairs: BTreeMap<String, BTreeSet<String>>,
    pub(crate) flights: SingleFlight<ContentKey, Result<FileDigest, String>>,
    /// Paired vertex shaders waiting for their translation and their pixel shaders.
    pub(crate) gates: Mutex<BTreeMap<String, Gate>>,
    /// Pixel shader → the paired vertex shaders waiting for it.
    pub(crate) dependents: Mutex<BTreeMap<String, Vec<String>>>,
    pub(crate) results: Mutex<BTreeMap<String, (String, String)>>,
    pub(crate) on_done: Option<crate::build::OnDone>,
    /// Store keys of the libraries this build used (`_b_cas.txt`).
    pub(crate) used: Mutex<BTreeSet<String>>,
    pub(crate) spans: Mutex<Vec<Span>>,
    pub(crate) t0: Instant,
}

impl Shared {
    pub(crate) fn result(&self, key: &str, st: &str, err: &str) {
        let err: String = err.chars().take(160).collect();
        self.results
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_string(), (st.to_string(), err));
        if let Some(f) = &self.on_done {
            (f.0)(key, st);
        }
    }

    pub(crate) fn span(&self, what: &'static str, key: &str, t: Instant) {
        self.spans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Span {
                what,
                key: key.to_string(),
                start: t.duration_since(self.t0),
                wall: t.elapsed(),
            });
    }

    /// A source's library, through single-flight and the store.
    pub(crate) fn library(&self, src: &str) -> Result<Libs, String> {
        let mac = self.lib_one(src)?;
        Ok(Libs { mac })
    }

    fn lib_one(&self, src: &str) -> Result<FileDigest, String> {
        let (conv, cid, tools, pch) = (
            self.conv,
            &self.compiler_id,
            &self.tools,
            self.pch.as_deref(),
        );
        let own = self.own.as_ref();
        let mut kb = ContentKey::builder("nxb.metallib.v2")
            .field_str("record", &conv.record())
            .field_str("compiler", cid)
            .field_str(
                "test-salt",
                &std::env::var("NXB_TEST_COMPILER_SALT").unwrap_or_default(),
            )
            .field("msl", src.as_bytes());
        if let Some(o) = own {
            kb = kb.field_str("frontend", o.key_field(src.as_bytes()));
        }
        // test hook for the chain-delta benchmark: a compiler identity change
        if let Ok(salt) = std::env::var("NXB_TEST_COMPILER_SALT") {
            kb = kb.field_str("test-salt", &salt);
        }
        let key = kb.finish();
        self.used
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_hex());
        let d = self.flights.get_or_run(key, || {
            if let Ok(Some(m)) = self.store.lookup(&key)
                && let Some(f) = m.files.first()
            {
                return Ok(f.digest);
            }
            let lib = crate::own_front::compile_link(
                tools,
                pch,
                own,
                conv.compile_flags,
                src.as_bytes(),
                self.io,
            )?
            .0;
            let mut w = self.store.entry(key);
            let d = w.add_bytes("b.metallib", &lib).map_err(|e| e.to_string())?;
            w.commit().map_err(|e| e.to_string())?;
            Ok(d)
        });
        (*d).clone()
    }

    fn lib_bytes(&self, d: &FileDigest) -> Result<Vec<u8>, String> {
        self.store.read_blob(&d.sha256).map_err(|e| e.to_string())
    }

    fn clone_to(&self, dir: &Path, name: &str, d: &FileDigest) -> Result<(), String> {
        let dst = dir.join(name);
        let _ = fs::remove_file(&dst);
        nxb_sys::clone::clone_or_copy(&self.store.blob_path(&d.sha256), &dst)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Put a source's library at its output path: a clone of the store's blob (a copy across volumes).
    pub(crate) fn place(&self, key: &str, suffix: &str, l: &Libs) -> Result<(), String> {
        let name = format!("{key}{suffix}");
        self.clone_to(&self.out, &name, &l.mac)
    }

    /// A pixel shader's side libraries: the unit compiled once and cut into `_b` and `_bx`, the functions
    /// of `names` going to `_bx`. The error is a status and its detail.
    pub(crate) fn side(
        &self,
        key: &str,
        src: &str,
        names: &[String],
    ) -> Result<(), (String, String)> {
        let metal = |e: String| ("metal failed".to_string(), e);
        let internal = |e: String| ("internal error".to_string(), e);
        let cut = |d: &FileDigest| {
            let b = self.lib_bytes(d).map_err(metal)?;
            side::cut(&b, names).map_err(|e| metal(e.to_string()))
        };
        let l = self.library(src).map_err(metal)?;
        let mac = cut(&l.mac)?;
        self.write(key, "_b.metallib", &mac.0).map_err(internal)?;
        self.write(key, "_bx.metallib", &mac.1).map_err(internal)?;
        Ok(())
    }

    /// `<key><suffix>.json` with `chain_metal` and `chain_compiler` (`MTL.with_compiler`).
    pub(crate) fn write_json(
        &self,
        key: &str,
        suffix: &str,
        mut refl: Value,
    ) -> Result<(), String> {
        refl.set("chain_metal", Value::str(self.conv.record()));
        refl.set("chain_compiler", Value::str(self.ident.clone()));
        self.write(
            key,
            &format!("{suffix}.json"),
            dumps_indent(&refl, 1).as_bytes(),
        )?;
        Ok(())
    }

    pub(crate) fn write(&self, key: &str, suffix: &str, bytes: &[u8]) -> Result<(), String> {
        let p: &Path = &self.out;
        fs::write(p.join(format!("{key}{suffix}")), bytes).map_err(|e| e.to_string())
    }
}

pub(crate) fn pairs(cfg: &crate::build::BuildConfig) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for t in ["shader_pairs.tsv", "pso_pairs.tsv"] {
        let mut p = cfg.profile.join(t);
        if t == "pso_pairs.tsv" && !p.is_file() {
            p = cfg.pkg.parent().map_or(p.clone(), |d| d.join(t));
        }
        for l in fs::read_to_string(&p).unwrap_or_default().lines() {
            if l.trim().is_empty() || l.starts_with('#') {
                continue;
            }
            let mut it = l.split_whitespace();
            if let (Some(v), Some(ps)) = (it.next(), it.next()) {
                out.entry(v.to_string()).or_default().insert(ps.to_string());
            }
        }
    }
    out
}
