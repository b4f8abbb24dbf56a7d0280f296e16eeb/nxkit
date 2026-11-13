//! In-memory orchestration of nxkit's unmodified split, job and compiler APIs.
use nxb_pyjson::{Value as J, dumps, parse};
type Result<T> = std::result::Result<T, String>;
const INPUT_LIMIT: usize = 256 * 1024 * 1024;
fn file(name: impl Into<String>, data: &[u8]) -> (String, Vec<u8>) {
    (name.into(), data.to_vec())
}
fn safe_name(s: &str) -> Result<()> {
    if s.is_empty()
        || s.len() > 128
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("invalid module or shader name".into());
    }
    Ok(())
}
pub(super) fn check_nso_size(nso: &[u8]) -> Result<()> {
    if !nso.starts_with(b"NSO0") {
        return Err("missing NSO0 header".into());
    }
    for i in 0..3 {
        let at = 0x14 + i * 16;
        let words = nso.get(at..at + 8).ok_or("truncated NSO")?;
        let offset =
            u32::from_le_bytes(words[..4].try_into().map_err(|_| "invalid NSO offset")?) as u64;
        let size =
            u32::from_le_bytes(words[4..].try_into().map_err(|_| "invalid NSO size")?) as u64;
        if offset + size > INPUT_LIMIT as u64 {
            return Err("NSO memory image exceeds 256 MiB".into());
        }
    }
    Ok(())
}
use nxb_split::{jobs, records, scanfile, shaders, src, unity, zstd};
use nxb_xlate::msl::{self, StageInputs};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const SHADER_LIMIT: usize = 20000;

struct Owned(Vec<u8>);
impl src::Src for Owned {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn bytes(&self) -> Option<&[u8]> {
        Some(&self.0)
    }
    fn read_at(
        &self,
        offset: u64,
        data: &mut [u8],
    ) -> std::result::Result<(), nxb_split::SplitError> {
        src::Mem(&self.0).read_at(offset, data)
    }
}
struct Work {
    job: nxb_xlate::job::Job,
    code: Vec<u8>,
    source: String,
    no_facts: bool,
}
fn work(value: &J, code: Vec<u8>, source: String) -> Result<Work> {
    let job = nxb_xlate::job::parse_jobs(&dumps(&J::List(vec![value.clone()])))?
        .into_iter()
        .next()
        .ok_or("missing shader job")?;
    safe_name(&job.key)?;
    Ok(Work {
        job,
        code,
        source,
        no_facts: value.get("no_facts") == Some(&J::Bool(true)),
    })
}

pub(super) fn discover(
    modules: &[(String, Vec<u8>)],
    resources: &[(String, Vec<u8>)],
    ios: bool,
) -> Result<Vec<(String, Vec<u8>)>> {
    let profile = shaders::Profile::default();
    let mut results = Vec::new();
    let mut notes = Vec::new();
    let mut common = None;
    let mut total = 0usize;
    for (name, nso) in modules {
        safe_name(name)?;
        check_nso_size(nso)?;
        let image = nxb_split::nso::image(nso).map_err(|e| format!("{name}: {e}"))?;
        if name == "main" {
            common = unity::bundle::common_strings(&image).ok();
        }
        let mut found = Vec::new();
        let mut variations = shaders::BahsVar::new();
        shaders::scan_blob(&image, name, &mut found, &mut variations, 0)
            .map_err(|e| e.to_string())?;
        found
            .extend(nxb_split::misc::main_extras(&image, name, &found).map_err(|e| e.to_string())?);
        let pairs = shaders::var_pairs(&profile, &found, &variations);
        results.push(records::FileResult {
            keys: records::uniq(&profile, found, true).map_err(|e| e.to_string())?,
            pairs,
            ..Default::default()
        });
    }
    let mut files = Vec::new();
    if resources.len() > 20000 {
        return Err("too many shader source files".into());
    }
    for (path, data) in resources {
        let rel = path.strip_prefix("romfs/").unwrap_or(path);
        total = total
            .checked_add(data.len())
            .ok_or("shader source size overflow")?;
        if total > INPUT_LIMIT {
            return Err("shader source files exceed 256 MiB".into());
        }
        files.push(jobs::File {
            rel: rel.into(),
            live: jobs::deferred(rel, &data[..data.len().min(16)]),
            src: Arc::new(Owned(data.clone())),
        });
    }
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    let mut dicts = zstd::Dicts::new();
    for file in &files {
        let raw = file.src.bytes().ok_or("shader source bytes unavailable")?;
        if raw.starts_with(&[0x37, 0xa4, 0x30, 0xec]) {
            dicts.add(raw).map_err(|e| e.to_string())?;
        } else if file.rel == "Pack/ZsDic.pack.zs" {
            let pack = zstd::decompress(raw).map_err(|e| e.to_string())?;
            for (_, data) in shaders::sarc_members(&pack, &file.rel).map_err(|e| e.to_string())? {
                if data.starts_with(&[0x37, 0xa4, 0x30, 0xec]) {
                    dicts.add(data).map_err(|e| e.to_string())?;
                }
            }
        }
    }
    let plan = jobs::plan(&files).map_err(|e| e.to_string())?;
    notes.extend(plan.notes);
    let ctx = jobs::Ctx {
        files: &files,
        dicts: &dicts,
    };
    for job in plan.jobs {
        let out = (job.run)(&ctx, &mut notes).map_err(|e| format!("{}: {e}", job.rel))?;
        if let Some(result) =
            scanfile::finish(&profile, out.found, &out.var).map_err(|e| e.to_string())?
        {
            results.push(result);
        }
    }
    let env = scanfile::Env {
        dicts: &dicts,
        common: common.as_ref(),
        trees: None,
    };
    let mut deferred = Vec::new();
    let mut plain = Vec::new();
    let mut trees = unity::ser::Trees::new();
    for (i, file) in files.iter().enumerate() {
        if plan.taken.contains(&i) {
            continue;
        }
        let label = format!("/{}", file.rel);
        let raw = file.src.bytes().ok_or("shader source bytes unavailable")?;
        let result = if file.live && raw.starts_with(&jobs::nlg::DICT) {
            let (found, var) = jobs::nlg::shaders(&files, i, &label).map_err(|e| e.to_string())?;
            scanfile::finish(&profile, found, &var)
        } else if file.live && jobs::nlg::covered(&file.rel, &files) {
            Ok(None)
        } else {
            scanfile::scan_file(
                &profile,
                file.src.as_ref(),
                &label,
                &file.rel,
                &env,
                &mut notes,
            )
        }
        .map_err(|e| format!("{}: {e}", file.rel))?;
        if let Some(result) = result {
            trees.extend(result.trees.clone());
            if result.defer {
                deferred.push(i);
            } else {
                plain.push((i, result));
            }
        }
    }
    if !deferred.is_empty() {
        for (key, value) in unity::ser::known_trees().map_err(|e| e.to_string())? {
            trees.entry(key).or_insert(value);
        }
        let env = scanfile::Env {
            trees: Some(&trees),
            ..env
        };
        for i in deferred {
            let file = &files[i];
            let result = scanfile::scan_file(
                &profile,
                file.src.as_ref(),
                &format!("/{}", file.rel),
                &file.rel,
                &env,
                &mut notes,
            )
            .map_err(|e| format!("{}: {e}", file.rel))?;
            if let Some(result) = result {
                if result.defer {
                    return Err(format!("{}: missing Unity shader type trees", file.rel));
                }
                plain.push((i, result));
            }
        }
    }
    // Deferred Unity scans keep their original file order at the merge.
    plain.sort_by_key(|(i, _)| *i);
    results.extend(plain.into_iter().map(|(_, result)| result));
    let output = records::build(&profile, &results).map_err(|e| e.to_string())?;
    if output.rows.len() > SHADER_LIMIT {
        return Err("too many discovered shaders".into());
    }
    let mut jobs = Vec::new();
    let mut offsets = BTreeMap::new();
    for (package, index, _, key, _) in &output.rows {
        let start = *offsets.entry(package).or_insert(0usize);
        let raw = output
            .packages
            .get(package)
            .and_then(|p| p.get(start..))
            .ok_or("missing shader package")?;
        let rd = |at: usize| -> Result<usize> {
            let bytes = raw.get(at..at + 4).ok_or("truncated shader package")?;
            Ok(u32::from_le_bytes(bytes.try_into().map_err(|_| "invalid shader size")?) as usize)
        };
        let first_size = rd(0x28)?;
        let second_size = rd(0x2c)?;
        let end = 0x38usize
            .checked_add(first_size)
            .and_then(|n| n.checked_add(second_size))
            .ok_or("shader size overflow")?;
        let code = raw.get(..end).ok_or("truncated shader code")?;
        let fact = output
            .facts
            .get(key)
            .map(|s| parse(s).map_err(|e| format!("{e:?}")))
            .transpose()?;
        let job =
            nxb_shader::job::job_for(key, package, code, fact.as_ref(), "", Default::default())
                .map_err(|e| e.to_string())?;
        notes.extend(job.notes);
        jobs.push(work(&job.json, job.code, format!("{package}#{index}"))?);
        offsets.insert(package, start + end);
    }
    let pairs_text = |pairs: &BTreeSet<(String, String)>| {
        pairs
            .iter()
            .map(|(a, b)| format!("{a}\t{b}\n"))
            .collect::<String>()
    };
    let map = output
        .rows
        .iter()
        .map(|(p, i, s, k, src)| format!("{p}\t{i}\t{s}\t{k}\t{src}\n"))
        .collect::<String>();
    let facts = output
        .facts
        .iter()
        .map(|(k, v)| format!("{k}\t{v}\n"))
        .collect::<String>();
    let tables = vec![
        file("shaders/shader_map.tsv", map.as_bytes()),
        file("shaders/shader_facts.tsv", facts.as_bytes()),
        file(
            "shaders/shader_pairs.tsv",
            pairs_text(&output.pairs).as_bytes(),
        ),
        file(
            "shaders/pso_pairs.tsv",
            pairs_text(&output.pso_pairs).as_bytes(),
        ),
    ];
    let pairs = output.pairs.union(&output.pso_pairs).cloned().collect();
    compile(jobs, &pairs, tables, notes, ios)
}

fn compile(
    mut jobs: Vec<Work>,
    pairs: &BTreeSet<(String, String)>,
    mut files: Vec<(String, Vec<u8>)>,
    notes: Vec<String>,
    ios: bool,
) -> Result<Vec<(String, Vec<u8>)>> {
    // The shared scanner marks unreadable or omitted content as RED. Generic builds have no partial mode.
    if let Some(note) = notes.iter().find(|note| note.starts_with("[RED]")) {
        return Err(format!("unsupported: incomplete shader discovery: {note}"));
    }
    let mut seen = BTreeSet::new();
    for entry in &jobs {
        if !seen.insert(&entry.job.key) {
            return Err(format!("duplicate shader key {}", entry.job.key));
        }
    }
    // Fragment inputs must be known before their vertex/mesh partners translate.
    jobs.sort_by(|a, b| (a.job.stage != "ps", &a.job.key).cmp(&(b.job.stage != "ps", &b.job.key)));
    let chain = nxb_shader::chain::standalone_version();
    let mut inputs = BTreeMap::<String, StageInputs>::new();
    for entry in jobs {
        let key = &entry.job.key;
        let partners = pairs
            .iter()
            .filter(|(v, _)| v == key)
            .filter_map(|(_, p)| inputs.get(p).cloned())
            .collect::<Vec<_>>();
        let ctx = msl::Context {
            chain: &chain,
            source: entry.source,
            partners: &partners,
            no_facts: entry.no_facts,
            ..Default::default()
        };
        let translated = msl::translate(&entry.job, &entry.code, &ctx)
            .map_err(|e| format!("shader {key}: {}: {}", e.reason, e.detail))?;
        inputs.insert(key.clone(), translated.inputs);
        for lib in translated.libraries {
            let stem = format!("shaders/{key}{}", lib.suffix);
            let mut metallib = compile_raw(lib.msl.as_bytes(), ios)
                .map_err(|e| format!("shader {key}{} compilation: {e:?}", lib.suffix))?;
            if lib.suffix == "_b"
                && let Some(side) = &translated.side
            {
                let names = side
                    .alpha
                    .iter()
                    .chain(&side.logic)
                    .cloned()
                    .collect::<Vec<_>>();
                if !names.is_empty() {
                    let (base, side) =
                        nxb_shader::side::cut(&metallib, &names).map_err(|e| e.to_string())?;
                    metallib = base;
                    files.push(file(format!("shaders/{key}_bx.metallib"), &side));
                }
            }
            files.push(file(format!("{stem}.metal"), lib.msl.as_bytes()));
            files.push(file(format!("{stem}.metallib"), &metallib));
            if let Some(reflection) = lib.reflection {
                files.push(file(
                    format!("{stem}.json"),
                    dumps(&reflection.to_value()).as_bytes(),
                ));
            }
        }
    }
    if !notes.is_empty() {
        files.push(file("shaders/warnings.txt", notes.join("\n").as_bytes()));
    }
    Ok(files)
}

// Keep compiler validation and panic handling in the shared tool layer.
#[cfg(feature = "air-front")]
fn compile_raw(msl: &[u8], ios: bool) -> Result<Vec<u8>> {
    use nxb_tool::FrontEnd;
    nxb_tool::airfront::RawFrontEnd::new(ios)
        .compile(msl)
        .map(|output| output.metallib)
        .map_err(|e| e.to_string())
}

#[cfg(not(feature = "air-front"))]
fn compile_raw(_msl: &[u8], _ios: bool) -> Result<Vec<u8>> {
    Err("unsupported: generic shader compilation requires the air-front feature".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unsafe_names_and_oversized_nso() {
        assert!(safe_name("../main").is_err());
        assert!(safe_name("main").is_ok());
        assert!(check_nso_size(b"NSO0").is_err());
        let mut nso = vec![0; 0x40];
        nso[..4].copy_from_slice(b"NSO0");
        nso[0x18..0x1c].copy_from_slice(&((INPUT_LIMIT as u32) + 1).to_le_bytes());
        assert!(check_nso_size(&nso).is_err());
    }
    #[test]
    fn incomplete_discovery_does_not_produce_a_successful_shader_stage() {
        let result = compile(
            vec![],
            &BTreeSet::new(),
            vec![],
            vec!["[RED] /content.pak: its shaders are left out".into()],
            false,
        );
        assert!(
            result
                .unwrap_err()
                .starts_with("unsupported: incomplete shader discovery")
        );
    }

    #[test]
    fn empty_discovery_produces_tables() {
        let files = discover(&[], &[], false).unwrap();
        assert!(
            files
                .iter()
                .any(|(name, _)| name == "shaders/shader_map.tsv")
        );
        assert!(!files.iter().any(|(name, _)| name.ends_with(".metallib")));
    }
}
