//! `nxb-archive --script <bases.mtl4-json> --arch <arch> --osmin <v> --store <dir> --work <dir> -o <out.mtl4a>
//! [--platform macos] [--target 1000] [--tokens N] [--cost 2] [--mem-gib 2.2] [--budget-gib G] [--pack-threads 1]
//! [--nt-jobs N]` (`--nt-jobs`: applegpu-nt -j N + air-pack instead of metal-tt)

use std::collections::BTreeMap;
use std::path::PathBuf;

/// `nxb archive` / `nxb-archive`: returns the exit code.
pub fn main(args: &[String]) -> i32 {
    let a = args;
    let mut o: BTreeMap<String, String> = BTreeMap::new();
    let mut i = 0;
    while i + 1 < a.len() {
        o.insert(a[i].trim_start_matches('-').to_string(), a[i + 1].clone());
        i += 2;
    }
    let g = |k: &str| o.get(k).cloned();
    let (Some(script), Some(arch), Some(osmin), Some(store), Some(work), Some(out)) = (
        g("script"),
        g("arch"),
        g("osmin"),
        g("store"),
        g("work"),
        g("o"),
    ) else {
        eprintln!("usage: see the header of nxb-archive.rs");
        return 2;
    };
    let platform = g("platform").unwrap_or_else(|| "macos".into());
    let sdk = if platform == "ios" {
        "iphoneos"
    } else {
        "macosx"
    };
    let find = |t: &str| crate::xcrun_find(sdk, t);
    let (Ok(metal_tt), Ok(metal_pack)) = (find("metal-tt"), find("metal-pack")) else {
        eprintln!("nxb-archive: metal-tt / metal-pack not found");
        return 1;
    };
    let sdkroot = std::process::Command::new("/usr/bin/xcrun")
        .args(["-sdk", sdk, "--show-sdk-path"])
        .output()
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or_default();
    let toolchain = std::process::Command::new(&metal_tt)
        .arg("-version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let num = |k: &str, d: f64| g(k).and_then(|v| v.parse().ok()).unwrap_or(d);
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8) as f64;
    let c = crate::Config {
        script: script.into(),
        arch,
        platform,
        osmin,
        sdkroot,
        metal_tt,
        metal_pack,
        toolchain,
        store: store.into(),
        work: work.into(),
        out: out.into(),
        target: num("target", 1000.0) as usize,
        tokens: num("tokens", cores) as u32,
        cost: num("cost", 2.0) as u32,
        mem_per_batch: (num("mem-gib", 2.2) * (1u64 << 30) as f64) as u64,
        mem_budget: (num(
            "budget-gib",
            nxb_sched::Budget::default_memory() as f64 / (1u64 << 30) as f64,
        ) * (1u64 << 30) as f64) as u64,
        pack_threads: num("pack-threads", 1.0) as u32,
        direct: None,
        nice: 0,
        progress: None,
        merge: true,
    };
    let mut c = c;
    if let Some(j) = g("nt-jobs").and_then(|v| v.parse().ok()) {
        match crate::Direct::find(&c.metal_tt, &c.toolchain, sdk, j) {
            Ok(d) => c.direct = Some(d),
            Err(e) => {
                eprintln!("nxb-archive: {e}");
                return 1;
            }
        }
    }
    match crate::build(&c) {
        Ok(r) => {
            let compiled: Vec<_> = r.batches.iter().filter(|b| !b.reused).collect();
            let cpu: f64 = compiled.iter().map(|b| b.cpu.as_secs_f64()).sum();
            let wall: f64 = compiled.iter().map(|b| b.wall.as_secs_f64()).sum();
            let rss = compiled.iter().map(|b| b.max_rss).max().unwrap_or(0);
            println!(
                "nxb-archive: {} batches ({} compiled, {} from the store), {} bytes; plan {:.1} s, compile {:.1} s, merge {:.1} s; metal-tt cpu {:.0} s over {:.0} s of batch wall (x{:.1} per batch), peak rss {:.2} GiB, peak reserved {:.1} GiB",
                r.batches.len(),
                compiled.len(),
                r.batches.len() - compiled.len(),
                r.size,
                r.seconds.0,
                r.seconds.1,
                r.seconds.2,
                cpu,
                wall,
                if wall > 0.0 { cpu / wall } else { 0.0 },
                rss as f64 / (1u64 << 30) as f64,
                r.peak_mem as f64 / (1u64 << 30) as f64
            );
            0
        }
        Err(e) => {
            eprintln!("nxb-archive: {e}");
            1
        }
    }
}
