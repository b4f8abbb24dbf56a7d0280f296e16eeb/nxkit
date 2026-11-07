//! Command line front end of `nxb-codesign`.

use std::path::{Path, PathBuf};

use crate::appstage;
use crate::bundle::{self, BundleOptions};
use crate::digest::{self, NoDigests, TsvDigests};
use crate::macho_sign::{Identifier, SignParams};
use crate::{Error, Result, cdhashes_of, sign_macho};

const USAGE: &str = "\
usage:
  nxb-codesign sign <bundle | mach-o file> [options]
      --entitlements <file.plist>   entitlements of the main executable
      --digests <file.tsv>          precomputed digests (see `digests`)
      --identifier <id>             explicit identifier (single Mach-O only)
      --no-sign-nested              seal nested code with its existing signature
      --runtime                     hardened runtime (--options runtime)
      --threads <n>                 hashing threads (default: all cores)
      --verbose                     print the phase timings
  nxb-codesign appstage <out.app> --manifest <file.tsv> [--threads <n>] [--entitlements <f>]
      [--no-hash-while-staging] [--verbose]
      stage the manifest's files (dest TAB source [TAB size TAB sha1 TAB sha256]) into a new
      bundle with parallel clones, then sign it
  nxb-codesign digests <path.app> [--threads <n>]   print the TSV digest list of the bundle
  nxb-codesign cdhash <mach-o file>                  print the cdhashes of a signed file

Ad-hoc only, output byte-compatible with `codesign --force --sign -`.";

/// `nxb sign` / `nxb-pack`: returns the exit code (0 done, 1 failed, 3 unsupported).
pub fn main(args: &[String]) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("nxb-codesign: {e}");
            match e {
                Error::Unsupported(_) => 3,
                _ => 1,
            }
        }
    }
}

struct Opts {
    path: PathBuf,
    entitlements: Option<PathBuf>,
    digests: Option<PathBuf>,
    manifest: Option<PathBuf>,
    hash_while_staging: bool,
    identifier: Option<String>,
    sign_nested: bool,
    runtime: bool,
    threads: usize,
    verbose: bool,
}

fn usage() -> Error {
    Error::Format(format!("bad arguments\n{USAGE}"))
}

fn parse(args: &[String]) -> Result<Opts> {
    let mut o = Opts {
        path: PathBuf::new(),
        entitlements: None,
        digests: None,
        manifest: None,
        hash_while_staging: true,
        identifier: None,
        sign_nested: true,
        runtime: false,
        threads: digest::default_threads(),
        verbose: false,
    };
    let mut it = args.iter();
    let mut path = None;
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or_else(usage);
        match a.as_str() {
            "--entitlements" => o.entitlements = Some(val()?.into()),
            "--digests" => o.digests = Some(val()?.into()),
            "--manifest" => o.manifest = Some(val()?.into()),
            "--no-hash-while-staging" => o.hash_while_staging = false,
            "--identifier" => o.identifier = Some(val()?),
            "--threads" => o.threads = val()?.parse().map_err(|_| usage())?,
            "--no-sign-nested" => o.sign_nested = false,
            "--runtime" => o.runtime = true,
            "--verbose" => o.verbose = true,
            s if !s.starts_with("--") && path.is_none() => path = Some(PathBuf::from(s)),
            _ => return Err(usage()),
        }
    }
    o.path = path.ok_or_else(usage)?;
    Ok(o)
}

fn read(p: &Path) -> Result<Vec<u8>> {
    std::fs::read(p).map_err(|e| Error::io(p, e))
}

fn run(args: &[String]) -> Result<()> {
    let (cmd, rest) = args.split_first().ok_or_else(usage)?;
    match cmd.as_str() {
        "sign" => {
            let o = parse(rest)?;
            let entitlements = o.entitlements.as_deref().map(read).transpose()?;
            if o.path.is_dir() {
                let tsv = o.digests.as_deref().map(TsvDigests::load).transpose()?;
                let opts = BundleOptions {
                    entitlements,
                    sign_nested: o.sign_nested,
                    runtime: o.runtime,
                    threads: o.threads,
                    digests: match &tsv {
                        Some(t) => t,
                        None => &NoDigests,
                    },
                };
                let r = bundle::sign_bundle(&o.path, &opts)?;
                println!(
                    "{}: signed ad hoc, identifier {}, cdhash {}, {} sealed, {} hashed ({} MB), {} digests reused, {} nested signed",
                    o.path.display(),
                    r.identifier,
                    r.cdhashes.last().map_or("-", |s| s.as_str()),
                    r.sealed,
                    r.hashed_files,
                    r.hashed_bytes / 1_000_000,
                    r.reused_digests,
                    r.nested_signed
                );
                if o.verbose {
                    for (name, d) in &r.timings {
                        println!("  {name:8} {:>9.3} ms", d.as_secs_f64() * 1000.0);
                    }
                }
            } else {
                let data = read(&o.path)?;
                let name = o
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("a.out");
                let mut params = SignParams::standalone(name);
                if let Some(id) = o.identifier {
                    params.identifier = Identifier::Explicit(id);
                }
                params.entitlements = entitlements;
                params.runtime = o.runtime;
                let signed = sign_macho(&data, &params)?;
                let tmp = o.path.with_extension("nxb-codesign.tmp");
                std::fs::write(&tmp, &signed.data).map_err(|e| Error::io(&tmp, e))?;
                let perm = std::fs::metadata(&o.path)
                    .map_err(|e| Error::io(&o.path, e))?
                    .permissions();
                std::fs::set_permissions(&tmp, perm).map_err(|e| Error::io(&tmp, e))?;
                std::fs::rename(&tmp, &o.path).map_err(|e| Error::io(&o.path, e))?;
                println!(
                    "{}: signed ad hoc, identifier {}",
                    o.path.display(),
                    signed.identifier
                );
            }
            Ok(())
        }
        "appstage" => {
            let o = parse(rest)?;
            let manifest = o.manifest.as_deref().ok_or_else(usage)?;
            let text = std::fs::read_to_string(manifest).map_err(|e| Error::io(manifest, e))?;
            let plan = appstage::parse_manifest(&text)?;
            let opts = appstage::StageOptions {
                threads: o.threads,
                hash_while_staging: o.hash_while_staging,
                entitlements: o.entitlements.as_deref().map(read).transpose()?,
                runtime: o.runtime,
                progress: None,
            };
            let r = appstage::stage_and_sign(&plan, &o.path, &opts)?;
            println!(
                "{}: staged {} files ({} MB) in {:.2} s, {} hashed while staging; signed in {:.2} s ({} digests reused, {} hashed), cdhash {}",
                o.path.display(),
                r.files,
                r.bytes / 1_000_000,
                r.stage_time.as_secs_f64(),
                r.hashed_while_staging,
                r.sign_time.as_secs_f64(),
                r.sign.reused_digests,
                r.sign.hashed_files,
                r.sign.cdhashes.last().map_or("-", |s| s.as_str())
            );
            if o.verbose {
                for (name, d) in &r.sign.timings {
                    println!("  {name:8} {:>9.3} ms", d.as_secs_f64() * 1000.0);
                }
            }
            Ok(())
        }
        "digests" => {
            let o = parse(rest)?;
            let mut out = String::new();
            for (p, size, d) in bundle::bundle_digests(&o.path, o.threads)? {
                out.push_str(&TsvDigests::line(&p, size, &d));
                out.push('\n');
            }
            print!("{out}");
            Ok(())
        }
        "cdhash" => {
            let o = parse(rest)?;
            for (t, h) in cdhashes_of(&read(&o.path)?)? {
                println!("{t:?} {}", crate::hash::hex(&h));
            }
            Ok(())
        }
        _ => Err(usage()),
    }
}
