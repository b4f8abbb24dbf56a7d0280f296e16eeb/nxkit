//! `mslcheck`: accept/reject verdicts for engine MSL files.
//!
//! ```text
//! mslcheck [options] [FILE ...]
//!   -l, --list FILE    read more paths from FILE (one per line)
//!   -j, --jobs N       worker threads (default: available parallelism)
//!   --json             one JSON object per line: path, ok, reasons (with counts), detail, sec, max_depth
//!   --sema             also check semantics after parser acceptance. No AIR or library is emitted
//!   --stats            with --json: also the construct counters
//!   --lexdump DIR      write the token dump of the n-th file to DIR/<n>.tok instead of checking
//!   -q, --quiet        no per-file lines, only the summary on stderr
//! ```
//!
//! Default output: `accept<TAB>path` or `reject<TAB>path<TAB>key<TAB>key...`, in input order,
//! with the reason keys spelled as the Python prototype spells them. A summary goes to stderr.
//! Exit status: 0 when every file was read, 2 on a read error or bad usage.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use msl_parse::Outcome;

/// Worker stack size. The emulated Python recursion limit bounds the parser's recursion far
/// below this.
const STACK: usize = 64 << 20;

struct Options {
    files: Vec<PathBuf>,
    jobs: usize,
    json: bool,
    stats: bool,
    quiet: bool,
    lexdump: Option<PathBuf>,
    sema: bool,
}

fn usage() -> ! {
    eprintln!(
        "usage: mslcheck [-l LIST] [-j N] [--json [--stats]] [--sema | --lexdump DIR] [-q] [FILE ...]"
    );
    std::process::exit(2)
}

fn parse_args() -> Options {
    let mut o = Options {
        files: Vec::new(),
        jobs: std::thread::available_parallelism().map_or(1, |n| n.get()),
        json: false,
        stats: false,
        quiet: false,
        lexdump: None,
        sema: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-l" | "--list" => {
                let list = args.next().unwrap_or_else(|| usage());
                let text = std::fs::read_to_string(&list).unwrap_or_else(|e| {
                    eprintln!("mslcheck: {list}: {e}");
                    std::process::exit(2)
                });
                o.files.extend(
                    text.lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(PathBuf::from),
                );
            }
            "-j" | "--jobs" => {
                o.jobs = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .filter(|&n| n > 0)
                    .unwrap_or_else(|| usage());
            }
            "--json" => o.json = true,
            "--stats" => o.stats = true,
            "--sema" => o.sema = true,
            "-q" | "--quiet" => o.quiet = true,
            "--lexdump" => o.lexdump = Some(PathBuf::from(args.next().unwrap_or_else(|| usage()))),
            "-h" | "--help" => usage(),
            _ if a.starts_with('-') && a.len() > 1 => usage(),
            _ => o.files.push(PathBuf::from(a)),
        }
    }
    if o.sema && o.lexdump.is_some() {
        usage();
    }
    o
}

/// JSON string literal.
fn json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn json_counts<'a>(it: impl Iterator<Item = (&'a str, u64)>, out: &mut String) {
    out.push('{');
    for (i, (k, v)) in it.enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        json_str(k, out);
        out.push_str(": ");
        out.push_str(&v.to_string());
    }
    out.push('}');
}

/// The parser verdict and, only after acceptance, the semantic verdict.
struct Check {
    parser: Outcome,
    semantic: Option<Result<(), String>>,
}

fn check(parser: Outcome, sema: bool) -> Check {
    let semantic = if sema && parser.accepted() {
        if let Outcome::Checked(c) = &parser {
            Some(
                msl_sema::analyze(&c.module)
                    .map(|_| ())
                    .map_err(|e| e.key()),
            )
        } else {
            None
        }
    } else {
        None
    };
    Check { parser, semantic }
}

/// One output line for a file: (line, accepted, read error).
fn render(
    path: &Path,
    res: &std::io::Result<Check>,
    sec: f64,
    o: &Options,
) -> (String, bool, bool) {
    let p = path.to_string_lossy();
    let mut line = String::new();
    let out = match res {
        Ok(out) => out,
        Err(e) => {
            if o.json {
                line.push_str("{\"path\": ");
                json_str(&p, &mut line);
                line.push_str(", \"error\": ");
                json_str(&e.to_string(), &mut line);
                if o.sema {
                    line.push_str(", \"phase\": \"read\"");
                }
                line.push_str("}\n");
            } else {
                line = format!("error\t{p}\t{e}\n");
            }
            return (line, false, true);
        }
    };
    let semantic_error = out.semantic.as_ref().and_then(|r| r.as_ref().err());
    let phase = if out.semantic.is_some() {
        "sema"
    } else {
        "parse"
    };
    let out = &out.parser;
    let ok = out.accepted() && semantic_error.is_none();
    let reasons = semantic_error.map_or_else(|| out.reasons(), |e| vec![(e.clone(), 1)]);
    if o.json {
        line.push_str("{\"path\": ");
        json_str(&p, &mut line);
        line.push_str(if ok {
            ", \"ok\": true"
        } else {
            ", \"ok\": false"
        });
        line.push_str(", \"reasons\": ");
        json_counts(reasons.iter().map(|(k, v)| (k.as_str(), *v)), &mut line);
        line.push_str(", \"detail\": ");
        json_str(
            &semantic_error.cloned().unwrap_or_else(|| out.detail()),
            &mut line,
        );
        if o.sema {
            line.push_str(", \"phase\": ");
            json_str(phase, &mut line);
        }
        if let Outcome::Checked(c) = out {
            if o.stats {
                line.push_str(", \"stats\": ");
                json_counts(c.stats.iter(), &mut line);
            }
            line.push_str(&format!(", \"max_depth\": {}", c.max_depth));
        } else if o.stats {
            line.push_str(", \"stats\": {}");
        }
        line.push_str(&format!(", \"sec\": {sec:.6}}}\n"));
    } else {
        line.push_str(if ok { "accept\t" } else { "reject\t" });
        line.push_str(&p);
        for (k, _) in &reasons {
            line.push('\t');
            line.push_str(k);
        }
        line.push('\n');
    }
    (line, ok, false)
}

fn lexdump(path: &Path, n: usize, dir: &Path) -> std::io::Result<()> {
    let src = msl_parse::read_source(path)?;
    let mut out = String::new();
    match msl_lex::tokenize(&src) {
        Ok(toks) => {
            for t in &toks {
                msl_lex::dump_line(t, &mut out);
            }
        }
        Err(e) => {
            out.push_str("LEXERROR\t");
            out.push_str(&e.message);
            out.push('\n');
        }
    }
    std::fs::write(dir.join(format!("{n}.tok")), out)
}

type Done = (usize, String, bool, bool);

fn main() {
    let o = Arc::new(parse_args());
    if o.files.is_empty() {
        usage();
    }
    if let Some(dir) = &o.lexdump {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("mslcheck: {}: {e}", dir.display());
            std::process::exit(2);
        }
    }
    if o.sema {
        eprintln!("mslcheck: parser and semantic checks only. No AIR, Metal compilation, or GPU validation.");
    }
    let t0 = Instant::now();
    let next = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::new();
    for _ in 0..o.jobs.min(o.files.len()) {
        let (o, next) = (o.clone(), next.clone());
        let h = std::thread::Builder::new()
            .stack_size(STACK)
            .spawn(move || {
                let mut done: Vec<Done> = Vec::new();
                loop {
                    let n = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = o.files.get(n) else { break };
                    if let Some(dir) = &o.lexdump {
                        match lexdump(path, n, dir) {
                            Ok(()) => done.push((n, String::new(), true, false)),
                            Err(e) => done.push((
                                n,
                                format!("error\t{}\t{e}\n", path.display()),
                                false,
                                true,
                            )),
                        }
                        continue;
                    }
                    let t = Instant::now();
                    let res = msl_parse::check_file(path).map(|p| check(p, o.sema));
                    let sec = t.elapsed().as_secs_f64();
                    let (line, ok, err) = render(path, &res, sec, &o);
                    done.push((n, line, ok, err));
                }
                done
            })
            .expect("spawn worker");
        workers.push(h);
    }
    let mut all: Vec<Done> = Vec::with_capacity(o.files.len());
    for w in workers {
        all.extend(w.join().expect("worker panicked"));
    }
    all.sort_unstable_by_key(|r| r.0);
    let wall = t0.elapsed().as_secs_f64();
    let stdout = std::io::stdout();
    let mut w = std::io::BufWriter::new(stdout.lock());
    let (mut accepted, mut errors) = (0usize, 0usize);
    for (_, line, ok, err) in &all {
        accepted += *ok as usize;
        errors += *err as usize;
        if !o.quiet {
            let _ = w.write_all(line.as_bytes());
        }
    }
    let _ = w.flush();
    if o.lexdump.is_some() {
        eprintln!(
            "mslcheck: {} token dumps, {} errors, {:.3} s",
            all.len(),
            errors,
            wall
        );
    } else {
        eprintln!(
            "mslcheck: {}/{} accepted, {} rejected, {} read errors, {:.3} s wall, {} threads",
            accepted,
            all.len(),
            all.len() - accepted - errors,
            errors,
            wall,
            o.jobs
        );
    }
    std::process::exit(if errors > 0 { 2 } else { 0 });
}

#[cfg(test)]
mod tests;
