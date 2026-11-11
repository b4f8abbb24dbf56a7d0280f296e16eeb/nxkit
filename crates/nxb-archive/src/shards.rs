//! The batch archives as shard files (`nxkit_abi::shards`): the pieces that `metal-tt` made are not merged into one
//! archive, each is a file of its own and an index tells the host which pipeline is in which one, so the host opens a
//! shard when it needs it and never reads the whole archive into memory.
//!
//! A shard is named by the content key of its batch (`<key>.mtl4archive`). The key covers the bases' content, so an
//! unchanged batch keeps its name and its file across rebuilds, and the folder of the previous build only loses the
//! files of batches that are gone.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use nxb_pyjson::Value;
use nxkit_abi::shards::{INDEX_FILE, IndexBuilder, SHARD_EXT};

use crate::cut::Base;
use crate::script::Script;

/// A library file stem and an entry name.
pub type Func = (String, String);

/// What the host asks a shard for: the functions of a pipeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ident {
    /// A render pipeline: the vertex and the fragment function.
    Render(Func, Func),
    /// A compute pipeline: its function.
    Compute(Func),
    /// A recorded state of a render pipeline, fully specialized (`crate::exact`): the vertex and the fragment function
    /// say which pair it belongs to.
    Exact(Func, Func),
}

/// One batch archive and what is in it.
#[derive(Clone, Debug)]
pub struct Shard {
    /// The batch's content key in hex.
    pub key: String,
    /// The batch archive (a blob of the store).
    pub path: PathBuf,
    /// The pipelines in it.
    pub idents: Vec<Ident>,
}

impl Shard {
    /// The file name in the shard folder.
    pub fn file(&self) -> String {
        format!("{}.{SHARD_EXT}", self.key)
    }
}

fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("script: no {k}"))
}

/// The pipelines of `batch`, as the host asks for them: the library is named by the file stem of its path (the host
/// loads `<stem>.metallib`), the function by its entry name.
pub fn idents(script: &Script, batch: &[Base]) -> Result<Vec<Ident>, String> {
    let by_label = |v: &'_ [Value]| -> HashMap<String, Value> {
        v.iter()
            .filter_map(|x| Some((text(x, "label").ok()?.to_string(), x.clone())))
            .collect()
    };
    let (fns, libs) = (by_label(&script.functions), by_label(&script.libraries));
    let func = |label: &str| -> Result<Func, String> {
        let label = label.trim_start_matches("fnd:");
        let f = fns
            .get(label)
            .ok_or_else(|| format!("script: no function {label}"))?;
        let lib = text(f, "library")?.trim_start_matches("lib:");
        let l = libs
            .get(lib)
            .ok_or_else(|| format!("script: no library {lib}"))?;
        let stem = Path::new(text(l, "path")?)
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("script: library {lib} has no file name"))?;
        Ok((stem.to_string(), text(f, "name")?.to_string()))
    };
    batch
        .iter()
        .map(|b| {
            if b.compute {
                let p = &script.computes[b.index];
                Ok(Ident::Compute(func(text(
                    p,
                    "compute_function_descriptor",
                )?)?))
            } else {
                let p = &script.pipelines[b.index];
                Ok(Ident::Render(
                    func(text(p, "vertex_function_descriptor")?)?,
                    func(text(p, "fragment_function_descriptor")?)?,
                ))
            }
        })
        .collect()
}

/// The pipelines of `batch` of exact states ([`idents`] for a batch of those): each is an [`Ident::Exact`].
pub fn exact_idents(script: &Script, batch: &[Base]) -> Result<Vec<Ident>, String> {
    Ok(idents(script, batch)?
        .into_iter()
        .map(|i| match i {
            Ident::Render(v, f) => Ident::Exact(v, f),
            other => other,
        })
        .collect())
}

/// The bytes of the index file of these shards, in the order given.
pub fn index_bytes(shards: &[Shard]) -> Vec<u8> {
    let mut ix = IndexBuilder::new();
    for s in shards {
        let n = ix.shard(&s.file());
        for id in &s.idents {
            match id {
                Ident::Render(v, f) => ix.render((&v.0, &v.1), (&f.0, &f.1), n),
                Ident::Compute(c) => ix.compute((&c.0, &c.1), n),
                Ident::Exact(v, f) => ix.exact((&v.0, &v.1), (&f.0, &f.1), n),
            }
        }
    }
    ix.encode()
}

/// What [`write()`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Written {
    /// Shard files in the folder.
    pub files: usize,
    /// Their bytes.
    pub bytes: u64,
    /// Files that were already there and kept.
    pub kept: usize,
    /// Files of batches that are gone, removed.
    pub removed: usize,
}

/// Puts the shards and the index into `dir`: a file of the same name and size is kept, another is linked from the
/// store (copied when the folder is on another volume), and a file that no shard names any more is removed. The index
/// is written last, through a temporary file, so a folder that was cut short has no index of the new set.
pub fn write(dir: &Path, shards: &[Shard]) -> Result<Written, String> {
    let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    let mut w = Written::default();
    let mut want: HashSet<String> = HashSet::from([INDEX_FILE.to_string()]);
    for s in shards {
        let (name, to) = (s.file(), dir.join(s.file()));
        let size = fs::metadata(&s.path).map_err(|e| io(&s.path, e))?.len();
        if !want.insert(name) {
            continue;
        }
        w.files += 1;
        w.bytes += size;
        if fs::metadata(&to).is_ok_and(|m| m.len() == size) {
            w.kept += 1;
            continue;
        }
        let _ = fs::remove_file(&to);
        if fs::hard_link(&s.path, &to).is_err() {
            let part = to.with_extension("partial");
            fs::copy(&s.path, &part).map_err(|e| io(&part, e))?;
            fs::rename(&part, &to).map_err(|e| io(&to, e))?;
        }
    }
    for e in fs::read_dir(dir).map_err(|e| io(dir, e))?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let stale = name.ends_with(&format!(".{SHARD_EXT}")) || name.ends_with(".partial");
        if stale && !want.contains(&name) {
            fs::remove_file(e.path()).map_err(|x| io(&e.path(), x))?;
            w.removed += 1;
        }
    }
    let (ix, part) = (
        dir.join(INDEX_FILE),
        dir.join(format!("{INDEX_FILE}.partial")),
    );
    fs::write(&part, index_bytes(shards)).map_err(|e| io(&part, e))?;
    fs::rename(&part, &ix).map_err(|e| io(&ix, e))?;
    Ok(w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_abi::shards::ShardIndex;

    const SCRIPT: &str = r#"{
 "libraries": [
  {"label": "v1_b", "path": "/lib/v1_b.metallib"},
  {"label": "f1_b", "path": "/lib/f1_b.metallib"},
  {"label": "k1_b", "path": "/lib/k1_b.metallib"}
 ],
 "function_descriptors": {"library_function_descriptors": [
  {"label": "v1_b.main0", "library": "lib:v1_b", "name": "main0"},
  {"label": "f1_b.main0", "library": "lib:f1_b", "name": "main0"},
  {"label": "k1_b.kern", "library": "lib:k1_b", "name": "kern"}
 ]},
 "pipeline_descriptors": {
  "render_pipeline_descriptors": [
   {"vertex_function_descriptor": "fnd:v1_b.main0", "fragment_function_descriptor": "fnd:f1_b.main0"}
  ],
  "compute_pipeline_descriptors": [
   {"compute_function_descriptor": "fnd:k1_b.kern"}
  ]
 }
}"#;

    fn base(index: usize, compute: bool) -> Base {
        Base {
            hash: [0; 32],
            group: [0; 32],
            index,
            compute,
        }
    }

    fn id(l: &str, e: &str) -> Func {
        (l.to_string(), e.to_string())
    }

    #[test]
    fn a_batch_is_named_the_way_the_host_asks() {
        let s = Script::parse(SCRIPT).unwrap();
        let got = idents(&s, &[base(0, false), base(0, true)]).unwrap();
        assert_eq!(
            got,
            vec![
                Ident::Render(id("v1_b", "main0"), id("f1_b", "main0")),
                Ident::Compute(id("k1_b", "kern")),
            ]
        );
    }

    #[test]
    fn a_function_the_script_lacks_is_an_error() {
        let s = Script::parse(&SCRIPT.replace("\"fnd:f1_b.main0\"", "\"fnd:gone\"")).unwrap();
        assert!(idents(&s, &[base(0, false)]).unwrap_err().contains("gone"));
    }

    fn shard(dir: &Path, key: &str, body: &[u8], idents: Vec<Ident>) -> Shard {
        let path = dir.join(format!("blob-{key}"));
        fs::write(&path, body).unwrap();
        Shard {
            key: key.into(),
            path,
            idents,
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nxb-shards-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_index_routes_every_pipeline_to_its_batch() {
        let d = tmp("index");
        let a = shard(
            &d,
            "aa",
            b"A",
            vec![Ident::Render(id("v1_b", "main0"), id("f1_b", "main0"))],
        );
        let b = shard(&d, "bb", b"BB", vec![Ident::Compute(id("k1_b", "kern"))]);
        let ix = ShardIndex::decode(&index_bytes(&[a, b])).unwrap();
        let r = ix.render(("v1_b", "main0"), ("f1_b", "main0")).unwrap();
        assert_eq!(ix.file(r), Some("aa.mtl4archive"));
        let c = ix.compute(("k1_b", "kern")).unwrap();
        assert_eq!(ix.file(c), Some("bb.mtl4archive"));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_rebuild_keeps_unchanged_files_and_drops_gone_batches() {
        let d = tmp("write");
        let out = d.join("pipelines");
        let (a, b) = (
            shard(&d, "aa", b"A", Vec::new()),
            shard(&d, "bb", b"BB", Vec::new()),
        );
        let first = write(&out, &[a.clone(), b]).unwrap();
        assert_eq!(
            (first.files, first.bytes, first.kept, first.removed),
            (2, 3, 0, 0)
        );
        assert_eq!(fs::read(out.join("bb.mtl4archive")).unwrap(), b"BB");
        // the next build has batch aa again and a new batch cc, and bb is gone
        let c = shard(&d, "cc", b"CCC", Vec::new());
        let second = write(&out, &[a, c]).unwrap();
        assert_eq!((second.files, second.kept, second.removed), (2, 1, 1));
        assert!(!out.join("bb.mtl4archive").exists());
        assert!(out.join("cc.mtl4archive").is_file());
        assert!(out.join(INDEX_FILE).is_file());
        assert!(!out.join(format!("{INDEX_FILE}.partial")).exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_missing_piece_fails_before_the_index_is_written() {
        let d = tmp("missing");
        let out = d.join("pipelines");
        let gone = Shard {
            key: "zz".into(),
            path: d.join("none"),
            idents: Vec::new(),
        };
        assert!(write(&out, &[gone]).is_err());
        assert!(!out.join(INDEX_FILE).exists());
        fs::remove_dir_all(&d).unwrap();
    }
}
