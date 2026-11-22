//! `airc`: drives the own MSL front end. The command line is [`main`]. The `airc` program is a thin binary over it
//! (and `nxb airc` calls it).
//!
//! ```text
//! airc emit IN.metal [-o OUT.ll]              lower one file to AIR text IR
//! airc check -l LIST [-j N] --work DIR        lower, air-as, metallib, air-validate every file; JSONL to stdout
//! airc rank -l LIST [-j N] [--results R.jsonl] [--top K]
//!                                             construct prevalence and greedy unlock order, Markdown to stdout
//! ```
//! Apple's tools are called directly with `SDKROOT` set (no `xcrun` per call), at most `-j` at once.

mod build;
mod check;
mod rank;
mod rawcheck;
mod sha256;

use std::path::PathBuf;

fn usage() -> i32 {
    eprintln!(
        "usage: airc emit|bc IN.metal [--ios] [-o OUT] | airc check -l LIST [-j N] --work DIR [--ios] [--keep] [--opt] [--ref-store DIR] | airc deny --results R.jsonl [-o DENY.txt] | airc build -l LIST --work DIR [--ios] [--direct] | airc rank -l LIST [--results R] [--top K] | airc repack LIB... | airc time -l LIST [--ios]"
    );
    2
}

/// `deny`: the deny list for nxkit's `DenyList` from `check --opt --ref-store` results: the SHA-256
/// of every source whose verdict is not `own-identical`, sorted, with a comment header.
fn deny(opts: &Opts) -> i32 {
    let Some(r) = &opts.results else {
        return usage();
    };
    let Ok(text) = std::fs::read_to_string(r) else {
        eprintln!("airc deny: cannot read {}", r.display());
        return 1;
    };
    let field = |l: &str, k: &str| -> Option<String> {
        let pat = format!("\"{k}\": \"");
        let i = l.find(&pat)? + pat.len();
        Some(l[i..].split('"').next()?.to_string())
    };
    let (mut denied, mut allowed) = (
        std::collections::BTreeSet::new(),
        std::collections::BTreeSet::new(),
    );
    for l in text.lines() {
        let (Some(sha), Some(v)) = (field(l, "source_sha256"), field(l, "verdict")) else {
            continue;
        };
        if v == "own-identical" {
            allowed.insert(sha);
        } else {
            denied.insert(sha);
        }
    }
    let what = if opts.allow {
        "allow list: sources whose library is byte-identical to Apple's (everything else goes to Apple)"
    } else {
        "deny list: sources whose library is not byte-identical to Apple's"
    };
    let mut out = format!(
        "# own front end {what}\n# front end {}, results {}\n# {} own-identical, {} not\n",
        air_front::id(),
        r.display(),
        allowed.len(),
        denied.len()
    );
    for d in if opts.allow { &allowed } else { &denied } {
        out.push_str(d);
        out.push('\n');
    }
    match &opts.out {
        Some(p) => {
            if std::fs::write(p, out).is_err() {
                return 1;
            }
        }
        None => print!("{out}"),
    }
    0
}

/// Options shared by the subcommands.
pub struct Opts {
    /// Input files.
    pub inputs: Vec<PathBuf>,
    /// Worker threads.
    pub jobs: usize,
    /// Output file of `emit`.
    pub out: Option<PathBuf>,
    /// Work directory of `check`.
    pub work: Option<PathBuf>,
    /// Lowering results for `rank`.
    pub results: Option<PathBuf>,
    /// Rows in the ranking.
    pub top: usize,
    /// Keep the intermediate files of `check`.
    pub keep: bool,
    /// `check`: back half `air-opt -O1`, `--discard-value-names`, `metallib` (instead of `air-as`).
    pub opt: bool,
    /// `build`: no optimiser (air-as | metallib).
    pub direct: bool,
    /// `check`: A3's reference store, to report byte equality of the final library.
    pub ref_store: Option<PathBuf>,
    /// The engine's iOS convention (`--ios`) instead of macOS.
    pub ios: bool,
    /// `deny --allow`: write the allow list (own-identical sources) instead.
    pub allow: bool,
}

impl Opts {
    /// The deployment target.
    pub fn target(&self) -> air_emit::Target {
        if self.ios {
            air_emit::Target::ios()
        } else {
            air_emit::Target::macos()
        }
    }

    /// SDK whose `SDKROOT` the Apple tools get.
    pub fn sdk(&self) -> &'static str {
        if self.ios {
            "iphoneos"
        } else {
            "macosx"
        }
    }
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        inputs: Vec::new(),
        jobs: 4,
        out: None,
        work: None,
        results: None,
        top: 60,
        keep: false,
        opt: false,
        ref_store: None,
        direct: false,
        ios: false,
        allow: false,
    };
    let mut k = 0;
    while k < args.len() {
        let a = args[k].clone();
        let mut val = || -> Result<String, String> {
            k += 1;
            args.get(k)
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "-l" => {
                let list = val()?;
                let text = std::fs::read_to_string(&list).map_err(|e| format!("{list}: {e}"))?;
                o.inputs.extend(
                    text.lines()
                        .filter(|l| !l.trim().is_empty())
                        .map(|l| PathBuf::from(l.trim())),
                );
            }
            "-j" => o.jobs = val()?.parse().map_err(|_| "bad -j")?,
            "-o" => o.out = Some(val()?.into()),
            "--work" => o.work = Some(val()?.into()),
            "--results" => o.results = Some(val()?.into()),
            "--top" => o.top = val()?.parse().map_err(|_| "bad --top")?,
            "--keep" => o.keep = true,
            "--opt" => o.opt = true,
            "--direct" => o.direct = true,
            "--ios" => o.ios = true,
            "--allow" => o.allow = true,
            "--ref-store" => o.ref_store = Some(val()?.into()),
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s => o.inputs.push(s.into()),
        }
        k += 1;
    }
    o.jobs = o.jobs.max(1);
    Ok(o)
}

fn emit_one(opts: &Opts) -> i32 {
    let Some(input) = opts.inputs.first() else {
        return usage();
    };
    let src = match msl_parse::read_source(input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("airc: {}: {e}", input.display());
            return 1;
        }
    };
    let name = "-".to_string();
    match air_emit::compile_source(&src, &opts.target(), &name) {
        Ok(ir) => {
            match &opts.out {
                Some(p) => {
                    if let Err(e) = std::fs::write(p, ir) {
                        eprintln!("airc: {}: {e}", p.display());
                        return 1;
                    }
                }
                None => print!("{ir}"),
            }
            0
        }
        Err(e) => {
            eprintln!("{e}");
            3
        }
    }
}

/// The `airc` command line: `args` are the arguments without the program name. Returns the exit code (0 success,
/// 2 usage, 3 lowering error, 4 bitcode error, 1 other failures). Output goes to the process stdout and stderr.
pub fn main(args: &[String]) -> i32 {
    let mut full = vec!["airc".to_string()];
    full.extend(args.iter().cloned());
    let args = full;
    let Some(cmd) = args.get(1) else {
        return usage();
    };
    let opts = match parse(&args[2..]) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("airc: {e}");
            return usage();
        }
    };
    match cmd.as_str() {
        "emit" => emit_one(&opts),
        "bc" => {
            let Some(input) = opts.inputs.first() else {
                return usage();
            };
            let Ok(src) = msl_parse::read_source(input) else {
                return 1;
            };
            // `.ll` input is text IR already (air-bc's subset), anything else is MSL
            let ir = if input.extension().is_some_and(|e| e == "ll") {
                src
            } else {
                match air_emit::compile_source(&src, &opts.target(), "-") {
                    Ok(ir) => ir,
                    Err(e) => {
                        eprintln!("{e}");
                        return 3;
                    }
                }
            };
            match air_bc::to_bitcode(&ir) {
                Ok(bc) => {
                    let out = opts.out.clone().unwrap_or_else(|| "out.bc".into());
                    if std::fs::write(&out, bc).is_err() {
                        return 1;
                    }
                    0
                }
                Err(e) => {
                    eprintln!("bitcode: {e}");
                    4
                }
            }
        }
        "deny" => deny(&opts),
        "rawcheck" => rawcheck::run(&opts),
        "rawcmp" => {
            // container tags of the raw library vs the library Apple's back half makes from the same IR
            let tools = match air_front::Tools::find(opts.sdk()) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("{e}");
                    return 1;
                }
            };
            let t = opts.target();
            let (mut same, mut diff, mut fail) = (0, 0, 0);
            for p in &opts.inputs {
                let Ok(b) = std::fs::read(p) else { continue };
                let (Ok(raw), Ok(apple)) = (
                    air_front::build_raw(&b, &t),
                    air_front::build(&b, &t, &tools),
                ) else {
                    fail += 1;
                    continue;
                };
                let (Ok(r), Ok(a)) = (
                    air_front::air_lib::parse(&raw),
                    air_front::air_lib::parse(&apple),
                ) else {
                    fail += 1;
                    continue;
                };
                let key = |l: &air_front::air_lib::Library| -> Vec<(
                    Vec<u8>,
                    Vec<air_front::air_lib::Tag>,
                    Vec<air_front::air_lib::Tag>,
                )> {
                    l.functions
                        .iter()
                        .map(|f| {
                            let t = f
                                .tags
                                .iter()
                                .filter(|t| !matches!(&t.0, b"HASH" | b"MDSZ" | b"OFFT"))
                                .cloned()
                                .collect();
                            (f.name().to_vec(), t, f.public.clone())
                        })
                        .collect()
                };
                if key(&r) == key(&a) && r.header == a.header {
                    same += 1;
                } else {
                    diff += 1;
                    println!("DIFF {}", p.display());
                }
            }
            println!("tags same {same} differ {diff} failed {fail}");
            0
        }
        "raw" => {
            // raw container path: no Apple tool
            let Some(input) = opts.inputs.first() else {
                return usage();
            };
            let Ok(b) = std::fs::read(input) else {
                return 1;
            };
            match air_front::build_raw(&b, &opts.target()) {
                Ok(lib) => {
                    let out = opts.out.clone().unwrap_or_else(|| "out.metallib".into());
                    if std::fs::write(&out, lib).is_err() {
                        return 1;
                    }
                    0
                }
                Err(Ok(r)) => {
                    eprintln!("{r}");
                    3
                }
                Err(Err(e)) => {
                    eprintln!("raw: {e}");
                    4
                }
            }
        }
        "time" => {
            // in-process front end cost: read, compile, bitcode, one thread, wall = CPU here
            let t = opts.target();
            let srcs: Vec<String> = opts
                .inputs
                .iter()
                .filter_map(|p| msl_parse::read_source(p).ok())
                .collect();
            let mut ms = Vec::new();
            let (mut ok, mut bytes, mut emit_ms, mut parse_ms) = (0usize, 0usize, 0.0f64, 0.0f64);
            for s in &srcs {
                let t0 = std::time::Instant::now();
                if let Ok(ir) = air_emit::compile_source(s, &t, "-") {
                    emit_ms += t0.elapsed().as_secs_f64() * 1000.0;
                    let t1 = std::time::Instant::now();
                    let m = air_bc::parse::parse(&ir);
                    parse_ms += t1.elapsed().as_secs_f64() * 1000.0;
                    if let Ok(bc) = m.and_then(|m| air_bc::writer::write(&m)) {
                        ok += 1;
                        bytes += bc.len();
                    }
                }
                ms.push(t0.elapsed().as_secs_f64() * 1000.0);
            }
            ms.sort_by(|a, b| a.total_cmp(b));
            let sum: f64 = ms.iter().sum();
            let med = ms.get(ms.len() / 2).copied().unwrap_or(0.0);
            println!(
                "{} inputs, {ok} to bitcode ({bytes} bytes): mean {:.2} ms (emit {:.2}, parse {:.2}), median {med:.2} ms, total {sum:.0} ms",
                ms.len(),
                sum / ms.len().max(1) as f64,
                emit_ms / ms.len().max(1) as f64,
                parse_ms / ms.len().max(1) as f64,
            );
            0
        }
        "repack" => {
            // parse and rewrite each metallib with air-lib, prints one line per file
            let mut bad = 0;
            for p in &opts.inputs {
                let Ok(b) = std::fs::read(p) else {
                    bad += 1;
                    continue;
                };
                let v = match air_lib::parse(&b) {
                    Ok(l) if air_lib::write(&l) == b => "identical".to_string(),
                    Ok(_) => "DIFF".to_string(),
                    Err(e) => format!("ERROR {e}"),
                };
                if v != "identical" {
                    bad += 1;
                }
                println!("{v} {}", p.display());
            }
            if bad == 0 {
                0
            } else {
                1
            }
        }
        "check" => check::run(&opts),
        "rank" => rank::run(&opts),
        "build" => build::run(&opts),
        _ => usage(),
    }
}
