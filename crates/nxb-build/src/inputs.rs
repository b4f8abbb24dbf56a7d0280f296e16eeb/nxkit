//! What a build reads the game from: a folder of packages (NSP, NSZ, XCI, XCZ, merged packages that hold the base
//! game, the update and DLC in one file) or a NUP container (the `.nup` file, or the only one in the folder).

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::Args;

mod generic;
pub(crate) use generic::discover_generic;

/// The kind of input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Packages: `nxb-extract` reads them and the split runs in the same pass.
    Packages,
    /// A NUP container (no keys).
    Nup,
}

/// The game found in the input.
#[derive(Clone, Debug)]
pub struct Inputs {
    /// The kind.
    pub source: Source,
    /// The title ID of the game (the base game's).
    pub app: u64,
    /// The base game's package or the NUP file.
    pub base: PathBuf,
    /// Base package version from metadata in generic mode. Zero in title-specific mode.
    pub base_version: u32,
    /// The updates by version (the package, the NUP file or `updates/<version>`).
    pub updates: Vec<(u32, PathBuf)>,
    /// DLC packages: (title ID, version, path). For a NUP source, its own path marks
    /// embedded DLC. Other paths name external packages.
    pub dlc: Vec<(u64, u32, PathBuf)>,
    /// The application to read from the packages (`nx_extract --app`): set for merged packages, cartridge images, a
    /// game picked with `--title-id`.
    pub app_flag: Option<u64>,
    /// Whether the NUP holds the base game's own files (version 0).
    pub has_base: bool,
    /// Notes for the console (skipped files, merged packages).
    pub notes: Vec<String>,
}

/// Package files of a folder (any letter case, no AppleDouble files).
const SUFFIXES: [&str; 4] = ["nsp", "nsz", "xci", "xcz"];

fn suffix(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn files_with(dir: &Path, sfx: &[&str]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && sfx.contains(&suffix(p).as_str())
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("._"))
        })
        .collect();
    v.sort();
    v
}

/// The file names of an NSP's PFS0 (empty for another file).
fn pfs0_names(path: &Path) -> Result<Vec<String>, String> {
    let bad = |m: &str| format!("{}: {m}", path.display());
    let mut f = std::fs::File::open(path).map_err(|e| bad(&e.to_string()))?;
    let mut h = [0u8; 16];
    if f.read_exact(&mut h).is_err() || &h[..4] != b"PFS0" {
        return Ok(Vec::new());
    }
    let n = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as usize;
    let st = u32::from_le_bytes([h[8], h[9], h[10], h[11]]) as usize;
    let len = f.metadata().map_err(|e| bad(&e.to_string()))?.len();
    if (n * 0x18 + st) as u64 > len.saturating_sub(16) {
        return Err(bad("truncated PFS0 file-name table"));
    }
    let mut ent = vec![0u8; n * 0x18];
    let mut strs = vec![0u8; st];
    f.read_exact(&mut ent)
        .and_then(|()| f.read_exact(&mut strs))
        .map_err(|e| bad(&e.to_string()))?;
    let mut out = Vec::new();
    for i in 0..n {
        let o = u32::from_le_bytes([
            ent[i * 0x18 + 0x10],
            ent[i * 0x18 + 0x11],
            ent[i * 0x18 + 0x12],
            ent[i * 0x18 + 0x13],
        ]) as usize;
        let tail = strs
            .get(o..)
            .ok_or_else(|| bad("a PFS0 file name lies outside the string table"))?;
        let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
        out.push(String::from_utf8_lossy(&tail[..end]).into_owned());
    }
    Ok(out)
}

/// An NSP/NSZ (PFS0) or an XCI/XCZ (`HEAD` at 0x100).
fn is_package(path: &Path) -> bool {
    let mut h = [0u8; 0x104];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut h))
        .is_ok()
        && (&h[..4] == b"PFS0" || &h[0x100..] == b"HEAD")
}

fn is_card(p: &Path) -> bool {
    matches!(suffix(p).as_str(), "xci" | "xcz")
}

fn hex16(s: &str) -> Option<u64> {
    (s.len() == 16 && s.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| u64::from_str_radix(s, 16).ok())
        .flatten()
}

/// `[<16 hex digits>]` in a file name.
fn name_tag(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    name.split('[')
        .skip(1)
        .filter_map(|t| t.split(']').next())
        .find_map(hex16)
}

/// `[v<number>]` in a file name.
fn name_version(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    name.split('[')
        .skip(1)
        .filter_map(|t| t.split(']').next())
        .find_map(|t| t.strip_prefix('v').and_then(|v| v.parse().ok()))
}

/// Content metas of a package (`nx_extract --contents`): (type, title ID, version).
type Metas = Vec<(String, u64, u32)>;

fn contents(keys: Option<&Path>, p: &Path) -> Result<Metas, String> {
    let keys = keys.ok_or_else(|| {
        crate::exit::fail(
            2,
            format!(
                "-k/--keys is required to read {} (a merged package, a card image or a file without a title ID in its name)",
                p.display()
            ),
        )
    })?;
    if !keys.is_file() {
        return Err(crate::exit::fail(
            6,
            format!(
                "the keys file {} does not exist (-k/--keys)",
                keys.display()
            ),
        ));
    }
    let text = nxb_extract::run::contents(keys, p)
        .map_err(|e| crate::exit::fail(e.exit_code(), format!("{}: {e}", p.display())))?;
    Ok(text
        .lines()
        .filter_map(|l| {
            let w: Vec<&str> = l.split(' ').collect();
            match w.as_slice() {
                ["content", t, id, v] if ["Application", "Patch", "AddOnContent"].contains(t) => {
                    Some((
                        t.to_string(),
                        u64::from_str_radix(id, 16).ok()?,
                        v.parse().ok()?,
                    ))
                }
                _ => None,
            }
        })
        .collect())
}

/// A family of packages by application.
#[derive(Default)]
struct Family {
    base: Option<PathBuf>,
    updates: Vec<(u32, PathBuf)>,
    dlc: Vec<(u64, u32, PathBuf)>,
}

fn scan_packages(
    dir: &Path,
    keys: Option<&Path>,
    want: Option<u64>,
    extra_dlc: &[PathBuf],
) -> Result<Inputs, String> {
    let mut notes = Vec::new();
    let mut fams: BTreeMap<u64, Family> = BTreeMap::new();
    let (mut unknown, mut merged_any, mut card_any) = (Vec::new(), false, false);
    let packages = files_with(dir, &SUFFIXES);
    let add = |fams: &mut BTreeMap<u64, Family>, p: &Path, tid: u64, ver: Option<u32>| {
        let low = tid & 0xFFF;
        let app = match low {
            0 => tid,
            0x800 => tid & !0xFFF,
            _ => (tid & !0xFFF).wrapping_sub(0x1000),
        };
        let f = fams.entry(app).or_default();
        match low {
            0 => f.base = Some(p.to_path_buf()),
            0x800 => f.updates.push((ver.unwrap_or(0), p.to_path_buf())),
            _ => f.dlc.push((tid, ver.unwrap_or(0), p.to_path_buf())),
        }
    };
    let all: Vec<PathBuf> = packages
        .iter()
        .cloned()
        .chain(extra_dlc.iter().cloned())
        .collect();
    for p in &all {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if std::fs::metadata(p).map(|m| m.len()).unwrap_or(0) == 0 {
            notes.push(format!("skipping an empty file: {name}"));
            continue;
        }
        if !is_package(p) {
            notes.push(format!(
                "skipping {name}: neither an NSP (PFS0) nor an XCI (HEAD at 0x100)"
            ));
            continue;
        }
        let plain = match suffix(p).as_str() {
            "nsz" => Some("nsp"),
            "xcz" => Some("xci"),
            _ => None,
        };
        if let Some(plain) = plain
            && let Some(b) = packages
                .iter()
                .find(|q| suffix(q) == plain && q.file_stem() == p.file_stem())
        {
            notes.push(format!(
                "skipping {name}: {} is beside it",
                b.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ));
            continue;
        }
        let names = if is_card(p) {
            Vec::new()
        } else {
            pfs0_names(p)?
        };
        let merged = names
            .iter()
            .filter(|n| n.to_lowercase().ends_with(".cnmt.nca"))
            .count()
            > 1;
        card_any |= is_card(p);
        if merged || is_card(p) {
            let cs = contents(keys, p)?;
            if !cs.is_empty() {
                if merged {
                    merged_any = true;
                    notes.push(format!(
                        "{name} holds several titles: {}",
                        cs.iter()
                            .map(|(t, i, v)| format!("{t} {i:016X} v{v}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                for (t, i, v) in cs {
                    add(&mut fams, p, i, (t != "Application").then_some(v));
                }
                continue;
            }
            notes.push(format!(
                "nxb: warning: {name}: its content metas could not be read, it counts as the one title its ticket or name gives"
            ));
        }
        let ticket = names.iter().find_map(|n| {
            n.strip_suffix(".tik")
                .or_else(|| n.strip_suffix(".TIK"))
                .filter(|s| s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()))
                .and_then(|s| hex16(&s[..16]))
        });
        let tid = match ticket.or_else(|| name_tag(p)) {
            Some(t) => Some(t),
            None => match keys {
                Some(_) => contents(keys, p)?.first().map(|c| c.1),
                None => None,
            },
        };
        let Some(tid) = tid else {
            unknown.push(p.clone());
            continue;
        };
        let version = name_version(p).or_else(|| {
            if tid & 0xFFF == 0 || keys.is_none() {
                None
            } else {
                contents(keys, p)
                    .ok()
                    .and_then(|c| c.iter().find(|m| m.1 == tid).map(|m| m.2))
            }
        });
        add(&mut fams, p, tid, version);
    }
    if fams.len() == 1
        && unknown.len() == 1
        && fams.values().all(|f| f.base.is_none())
        && let Some(f) = fams.values_mut().next()
    {
        f.base = unknown.pop();
    }
    if fams.is_empty() {
        return Err(format!(
            "no game package (.nsp, .nsz, .xci, .xcz, .nup) with a readable title ID in {}",
            dir.display()
        ));
    }
    let games = fams
        .keys()
        .map(|t| format!("{t:016X}"))
        .collect::<Vec<_>>()
        .join(", ");
    if let Some(w) = want
        && !fams.contains_key(&w)
    {
        return Err(format!(
            "no game {w:016X} (--title-id) in {}, which holds {games}",
            dir.display()
        ));
    }
    if want.is_none() && fams.len() > 1 {
        return Err(format!(
            "more than one game in the folder: {games}; keep one game per folder, or name the one to pack with --title-id"
        ));
    }
    let app = want.unwrap_or_else(|| fams.keys().next().copied().unwrap_or(0));
    let fam = fams.remove(&app).unwrap_or_default();
    let base = fam.base.ok_or_else(|| {
        format!(
            "no base game package found in {} (title ID {app:016X})",
            dir.display()
        )
    })?;
    // one DLC in several copies: the highest version, then the largest file
    let mut best: BTreeMap<u64, (u32, PathBuf)> = BTreeMap::new();
    for (t, v, p) in fam.dlc {
        let size = |x: &Path| std::fs::metadata(x).map(|m| m.len()).unwrap_or(0);
        if best
            .get(&t)
            .is_none_or(|(bv, bp)| (v, size(&p)) > (*bv, size(bp)))
        {
            best.insert(t, (v, p));
        }
    }
    let mut updates = fam.updates;
    updates.sort();
    updates.dedup_by_key(|u| u.0);
    Ok(Inputs {
        source: Source::Packages,
        app,
        base,
        base_version: 0,
        updates,
        dlc: best.into_iter().map(|(t, (v, p))| (t, v, p)).collect(),
        app_flag: (merged_any || card_any || want.is_some()).then_some(app),
        has_base: true,
        notes,
    })
}

fn scan_nup(path: &Path, want: Option<u64>) -> Result<Inputs, String> {
    let n = nxb_extract::nup::Nup::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let s = nxb_extract::nup::scan(&n).map_err(|e| e.to_string())?;
    if let Some(w) = want
        && w != s.app
    {
        return Err(format!(
            "no game {w:016X} (--title-id) in {}, which holds {:016X}",
            path.display(),
            s.app
        ));
    }
    Ok(Inputs {
        source: Source::Nup,
        app: s.app,
        base: path.to_path_buf(),
        base_version: 0,
        updates: s.updates.keys().map(|v| (*v, path.to_path_buf())).collect(),
        dlc: s.dlc.keys().map(|t| (*t, 0, path.to_path_buf())).collect(),
        app_flag: None,
        has_base: s.base.is_some(),
        notes: Vec::new(),
    })
}

fn parse_title(s: &str) -> Result<u64, String> {
    hex16(s.trim()).ok_or_else(|| {
        crate::exit::fail(
            2,
            format!("--title-id / --app: 16 hex digits expected, not {s}"),
        )
    })
}

/// Finds the game in the input the command line names: `--nup`, a `.nup` file, or a folder (packages, else
/// the one NUP in it).
pub fn discover(a: &Args) -> Result<Inputs, String> {
    let want = a.title_id.as_deref().map(parse_title).transpose()?;
    let keys = a.keys.as_deref();
    if let Some(n) = &a.nup {
        return nup_checked(n).and_then(|n| scan_nup(&n, want));
    }
    let p = &a.nsp_dir;
    if p.is_file() {
        return nup_checked(p).and_then(|n| scan_nup(&n, want));
    }
    if !p.is_dir() {
        return Err(format!("{} is not a folder or a NUP file", p.display()));
    }
    if files_with(p, &SUFFIXES).is_empty() && a.aoc.is_empty() {
        let nups: Vec<PathBuf> = files_with(p, &["nup"])
            .into_iter()
            .filter(|n| nxb_extract::nup::is_nup(n))
            .collect();
        match nups.as_slice() {
            [one] => {
                return scan_nup(one, want);
            }
            [] => {}
            many => {
                return Err(format!(
                    "more than one NUP in {}: {}; keep one game per folder, or give the .nup file",
                    p.display(),
                    many.iter()
                        .filter_map(|n| n.file_name().map(|f| f.to_string_lossy().into_owned()))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }
    scan_packages(p, keys, want, &a.aoc)
}

fn nup_checked(p: &Path) -> Result<PathBuf, String> {
    if nxb_extract::nup::is_nup(p) {
        Ok(p.to_path_buf())
    } else {
        Err(format!("not a folder or a NUP file: {}", p.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_in_file_names() {
        let p = Path::new("Game [0100ABCD00000000][v65536].nsp");
        assert_eq!(name_tag(p), Some(0x0100_ABCD_0000_0000));
        assert_eq!(name_version(p), Some(65536));
        assert_eq!(name_tag(Path::new("Game.nsp")), None);
        assert_eq!(suffix(Path::new("A.NSZ")), "nsz");
    }
}
