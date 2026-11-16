//! The rewrite stage of the standalone build: every module of the title through `nxb-rewrite`, with the title's own
//! rules, then its parts (segments, imports, exports).

use super::elf::{self, Symbols};
use super::layout::{self, Fixups, Shape};
use nxb_rewrite::{Options, RewriteError, nso, patches::PatchTable, rewrite};
use nxtitle::Standalone;
use std::path::Path;

/// One rewritten module.
pub(crate) struct Built {
    /// The module's name (`main`, `subsdk0`, `sdk`).
    pub name: String,
    /// The first 16 bytes of the build ID as 32 hex digits.
    pub build_id: String,
    /// Where the parts go.
    pub shape: Shape,
    /// `(offset, size)` of text, rodata and data in the module's memory.
    pub parts: [(usize, usize); 3],
    /// The bss size.
    pub bss: usize,
    /// The rewritten flat image (`<name>.patched.bin`).
    pub image: Vec<u8>,
    /// Imports and exports of the original module.
    pub symbols: Symbols,
    /// The `.fixups.tsv` text: the host needs the key page and the gate indices from its header.
    pub fixups: String,
    /// The `.gates.tsv` bytes.
    pub gates: Vec<u8>,
}

fn err(name: &str, e: RewriteError) -> String {
    match e {
        RewriteError::Unsupported(m) => format!("unsupported: module {name}: {m}"),
        RewriteError::Failed(m) => format!("module {name}: {m}"),
    }
}

/// Rewrites the modules of `st` from the ExeFS folder `exefs`. `rules` is the title's rule file text.
pub(crate) fn rewrite_all(
    exefs: &Path,
    st: &Standalone,
    profile: &str,
    rules: &str,
    say: impl FnMut(&str),
    progress: impl FnMut(u64, u64, &str),
) -> Result<Vec<Built>, String> {
    rewrite_with_loader(
        st.modules,
        profile,
        rules,
        |name| {
            let path = exefs.join(name);
            std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
        },
        say,
        progress,
    )
}

/// Rewrites ordered modules loaded by the caller.
pub(crate) fn rewrite_with_loader(
    modules: &[(&str, &str)],
    profile: &str,
    rules: &str,
    mut load: impl FnMut(&str) -> Result<Vec<u8>, String>,
    mut say: impl FnMut(&str),
    mut progress: impl FnMut(u64, u64, &str),
) -> Result<Vec<Built>, String> {
    let table = PatchTable::from_toml(&[(format!("{profile}.toml"), rules.to_string())])
        .map_err(|e| err("rules", e))?;
    let mut out = Vec::new();
    for (index, (name, want_id)) in modules.iter().enumerate() {
        progress(index as u64, modules.len() as u64, name);
        let data = load(name)?;
        let m = nso::load(&data, name).map_err(|e| err(name, e))?;
        let id = nxb_model::to_hex(&m.build_id[..16]);
        if id != *want_id {
            return Err(format!(
                "module {name}: build ID {id} is not the one the title module knows ({want_id}): this is another version of the game"
            ));
        }
        let symbols = elf::symbols(&m.image).map_err(|e| format!("module {name}: {e}"))?;
        let opts = Options {
            name: (*name).to_string(),
            profile: profile.to_string(),
            snowdrop_from: "default, no snowdrop pools".to_string(),
            ..Options::default()
        };
        let o = rewrite(&data, &opts, &table).map_err(|e| err(name, e))?;
        for l in &o.log {
            say(l);
        }
        let file = |suffix: &str| {
            let want = format!("{name}.{suffix}");
            o.files
                .iter()
                .find(|(n, _)| *n == want)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| format!("module {name}: the rewriter wrote no {want}"))
        };
        let image = file("patched.bin")?;
        let fixups = String::from_utf8(file("fixups.tsv")?)
            .map_err(|_| format!("module {name}: {name}.fixups.tsv is not UTF-8"))?;
        let gates = file("gates.tsv")?;
        let shape = layout::shape(name, index, image.len(), &Fixups::parse(&fixups))?;
        out.push(Built {
            name: (*name).to_string(),
            build_id: id,
            shape,
            parts: m.segments,
            bss: m.bss,
            image,
            symbols,
            fixups,
            gates,
        });
    }
    Ok(out)
}
