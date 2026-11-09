//! `nxb self-check`: find the external tools the stages call and print their versions.
//!
//! One line per tool: `ok <name> <path> <version>` or `missing <name> (required|optional)`. Exit code 0 when every
//! required tool is found, 1 when one is missing. Apple tools are found with `xcrun -sdk <sdk> -f`, the others on
//! `PATH`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

enum Find {
    Xcrun(&'static str),
    Path,
}

struct Tool {
    name: &'static str,
    find: Find,
    required: bool,
    version: Option<&'static [&'static str]>,
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn locate(f: &Find, name: &str) -> Option<PathBuf> {
    match f {
        Find::Xcrun(sdk) => {
            let o = Command::new("/usr/bin/xcrun")
                .args(["-sdk", sdk, "-f", name])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()?;
            let p = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
            (o.status.success() && p.is_file()).then_some(p)
        }
        Find::Path => which(name),
    }
}

/// The first output line of `path args`, or "" when it does not answer within 10 seconds.
fn version(path: &PathBuf, args: &[&str]) -> String {
    let Ok(mut ch) = Command::new(path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
        return String::new();
    };
    let t = Instant::now();
    while matches!(ch.try_wait(), Ok(None)) {
        if t.elapsed() > Duration::from_secs(10) {
            let _ = ch.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let Ok(o) = ch.wait_with_output() else {
        return String::new();
    };
    [o.stdout, o.stderr]
        .iter()
        .flat_map(|b| {
            String::from_utf8_lossy(b)
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn tools() -> Vec<Tool> {
    let t = |name, find, required, version| Tool {
        name,
        find,
        required,
        version,
    };
    vec![
        t(
            "metal",
            Find::Xcrun("macosx"),
            true,
            Some(&["--version"][..]),
        ),
        t(
            "metallib",
            Find::Xcrun("macosx"),
            true,
            Some(&["--version"][..]),
        ),
        t(
            "clang",
            Find::Xcrun("macosx"),
            true,
            Some(&["--version"][..]),
        ),
        t("metal-tt", Find::Xcrun("macosx"), false, None),
        t("metal-pack", Find::Xcrun("macosx"), false, None),
        t("codesign", Find::Path, false, None),
    ]
}

/// Run the command.
pub(crate) fn main(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if !args.is_empty() {
        let _ = writeln!(err, "usage: nxb self-check");
        return 2;
    }
    let mut missing = 0;
    for tool in tools() {
        let bare = tool.name.split(' ').next().unwrap_or(tool.name);
        let line = match locate(&tool.find, bare) {
            Some(p) => {
                let ver = tool.version.map(|a| version(&p, a)).unwrap_or_default();
                format!("ok       {:<22} {} {ver}", tool.name, p.display())
            }
            None => {
                if tool.required {
                    missing += 1;
                }
                let kind = if tool.required {
                    "required"
                } else {
                    "optional"
                };
                format!("missing  {:<22} ({kind})", tool.name)
            }
        };
        if writeln!(out, "{}", line.trim_end()).is_err() {
            return 1;
        }
    }
    i32::from(missing > 0)
}
