//! Opening the versions of a game: the base program alone (`v0`) and the base under each update, the control data
//! (`control.nacp`, icon, name) and the ExeFS, with the stdout lines `nx_extract` prints (the NSP script parses them).

use std::sync::Arc;

use crate::bktr::patched_section;
use crate::cache::BlockCache;
use crate::error::{ExtractError, Result};
use crate::ivfc::{IvfcStorage, read_sha256_section};
use crate::keys::KeySet;
use crate::nca::{ContentType, Nca, PfsEntry, pick, read_pfs0};
use crate::romfs::{RomEntry, read_file, read_tables};
use crate::storage::{Counter, CtrSection, MemStorage, Storage};

/// The checked RomFS of one version with its tables.
pub struct VersionFs {
    /// The checked data level.
    pub fs: Arc<IvfcStorage>,
    /// Every entry of its tables.
    pub entries: Vec<RomEntry>,
    /// For an update: the relocation view, to tell which reads come from the base.
    pub indirect: Option<Arc<crate::storage::IndirectStorage>>,
}

/// A section decrypted with its plain counter (or not encrypted).
pub fn plain_section(
    ks: &KeySet,
    nca: &Nca,
    i: usize,
    cache: &Arc<BlockCache>,
) -> Result<(Arc<dyn Storage>, crate::nca::FsHeader)> {
    let (raw, start, fs) = nca.section_view(ks, i)?;
    let s: Arc<dyn Storage> = match fs.encryption {
        1 => raw,
        3 => Arc::new(CtrSection::new(
            raw,
            nca.ctr_key(ks)?,
            start,
            Counter::Upper(fs.upper),
            cache.clone(),
        )),
        x => {
            return Err(ExtractError::Unsupported(format!(
                "{}: section {i} with encryption type {x}",
                nca.id
            )));
        }
    };
    Ok((s, fs))
}

/// The base program's decrypted data section, if it has one.
pub fn base_data(
    ks: &KeySet,
    base: &Nca,
    cache: &Arc<BlockCache>,
) -> Result<Option<(Arc<dyn Storage>, crate::nca::FsHeader)>> {
    let i = base.data_section();
    if base.sections[i].is_some() {
        Ok(Some(plain_section(ks, base, i, cache)?))
    } else {
        Ok(None)
    }
}

/// Opens the RomFS of the base alone, or patched by `update`.
/// `base_data` is the base program's decrypted data section ([`base_data`]); every version shares the one instance,
/// so its chunks are decrypted once for all of them.
pub fn open_romfs(
    ks: &KeySet,
    base_data: Option<(Arc<dyn Storage>, crate::nca::FsHeader)>,
    update: Option<&Nca>,
    cache: &Arc<BlockCache>,
) -> Result<VersionFs> {
    open_romfs_with(ks, base_data, update, cache, false)
}

/// [`open_romfs`]; with `lenient` (`--allow-damaged`) data blocks that fail their hash are read as they are.
pub fn open_romfs_with(
    ks: &KeySet,
    base_data: Option<(Arc<dyn Storage>, crate::nca::FsHeader)>,
    update: Option<&Nca>,
    cache: &Arc<BlockCache>,
    lenient: bool,
) -> Result<VersionFs> {
    match update {
        None => {
            let Some((data, fs)) = base_data else {
                return Ok(VersionFs {
                    fs: Arc::new(IvfcStorage::empty()),
                    entries: Vec::new(),
                    indirect: None,
                });
            };
            let ivfc = Arc::new(IvfcStorage::open_with(data, &fs, lenient)?);
            let entries = read_tables(ivfc.as_ref())?;
            Ok(VersionFs {
                fs: ivfc,
                entries,
                indirect: None,
            })
        }
        Some(upd) => {
            let (raw, start, fs) = upd.section_view(ks, upd.data_section())?;
            if fs.encryption != 4 {
                return Err(ExtractError::Unsupported(format!(
                    "update data section with encryption type {}",
                    fs.encryption
                )));
            }
            let (base_storage, _) = base_data
                .ok_or_else(|| ExtractError::Unsupported("sparse or missing base data".into()))?;
            let (ind, _patch) = patched_section(
                base_storage,
                raw,
                start,
                &fs,
                || upd.ctr_key(ks),
                cache.clone(),
            )?;
            let ind = Arc::new(ind);
            let view: Arc<dyn Storage> = ind.clone();
            let ivfc = Arc::new(IvfcStorage::open_with(view, &fs, lenient)?);
            let entries = read_tables(ivfc.as_ref())?;
            Ok(VersionFs {
                fs: ivfc,
                entries,
                indirect: Some(ind),
            })
        }
    }
}

/// The game's language names in NACP order (icon file names).
pub const ICON_LANGS: [&str; 16] = [
    "AmericanEnglish",
    "BritishEnglish",
    "Japanese",
    "French",
    "German",
    "LatinAmericanSpanish",
    "Spanish",
    "Italian",
    "Dutch",
    "CanadianFrench",
    "Portuguese",
    "Russian",
    "Korean",
    "TraditionalChinese",
    "SimplifiedChinese",
    "BrazilianPortuguese",
];

/// The control data: the whole `control.nacp` and the first icon in `ICON_LANGS` order.
pub struct Control {
    /// `control.nacp`.
    pub nacp: Vec<u8>,
    /// `(language, JPEG)`.
    pub icon: Option<(&'static str, Vec<u8>)>,
}

/// Reads the control NCA's RomFS.
pub fn read_control(ks: &KeySet, ctrl: &Nca, cache: &Arc<BlockCache>) -> Result<Control> {
    let (data, fs) = plain_section(ks, ctrl, 0, cache)?;
    let ivfc = IvfcStorage::open(data, &fs)?;
    let entries = read_tables(&ivfc)?;
    let nacp = read_file(&ivfc, &entries, "/control.nacp")?
        .ok_or_else(|| ExtractError::format("no /control.nacp"))?;
    let mut icon = None;
    for lang in ICON_LANGS {
        if let Some(v) = read_file(&ivfc, &entries, &format!("/icon_{lang}.dat"))? {
            icon = Some((lang, v));
            break;
        }
    }
    Ok(Control { nacp, icon })
}

/// The ExeFS files (name, bytes), in PFS0 order.
pub fn read_exefs(
    ks: &KeySet,
    prog: &Nca,
    cache: &Arc<BlockCache>,
) -> Result<Vec<(String, Vec<u8>)>> {
    let (sec, fs) = plain_section(ks, prog, 0, cache)?;
    let data = read_sha256_section(sec.as_ref(), &fs)?;
    let mem = MemStorage(data);
    let list: Vec<PfsEntry> = read_pfs0(&mem)?;
    // names are checked before any is used as a file name (`ExeFS/<name>`)
    for e in &list {
        crate::safe_path::rel("ExeFS", &e.name, true)?;
    }
    list.into_iter()
        .map(|e| Ok((e.name, mem.read_vec(e.offset, e.size as usize)?)))
        .collect()
}

/// `TitleName` of `nx_extract`: the first non-empty name of the NACP's language entries (deflated NACPs: not yet).
pub fn title_name(nacp: &[u8]) -> Result<String> {
    let n = nacp.first().copied().unwrap_or(0) as usize
        | (nacp.get(1).copied().unwrap_or(0) as usize) << 8;
    // newer SDKs deflate the 0x3000 bytes of names: a u16 size, then raw deflate of 32 entries of 0x300 bytes
    let inflated = if n > 0 && n <= 0x2ffe {
        nacp.get(2..2 + n)
            .and_then(|z| crate::inflate::inflate(z, 0x6000))
            .filter(|t| t.len() == 0x6000)
    } else {
        None
    };
    let (t, entries) = match &inflated {
        Some(t) => (t.as_slice(), 32),
        None => (nacp, 16),
    };
    for i in 0..entries {
        let s = t.get(i * 0x300..i * 0x300 + 0x200).unwrap_or(&[]);
        let len = s.iter().position(|&b| b == 0).unwrap_or(s.len());
        if len > 0 {
            return Ok(String::from_utf8_lossy(&s[..len]).into_owned());
        }
    }
    Ok(String::new())
}

/// `display version` from the NACP.
pub fn display_version(nacp: &[u8]) -> String {
    let s = nacp.get(0x3060..0x3070).unwrap_or(&[]);
    String::from_utf8_lossy(s)
        .trim_end_matches('\0')
        .to_string()
}

/// The Program NCA of the base, of an update (same title), and the Control NCA, as `nx_extract` picks them.
pub fn pick_programs<'a>(
    base: &'a [Nca],
    update: Option<&'a [Nca]>,
) -> Result<(&'a Nca, Option<&'a Nca>, Option<&'a Nca>)> {
    pick_program(base, update, None)
}

/// [`pick_programs`] for one program of a multi-program application (`nx_extract --program=<title ID>`): the Program NCA
/// of that title ID, or the lowest one without it.
pub fn pick_program<'a>(
    base: &'a [Nca],
    update: Option<&'a [Nca]>,
    program: Option<u64>,
) -> Result<(&'a Nca, Option<&'a Nca>, Option<&'a Nca>)> {
    let bp = pick(base, ContentType::Program, program).ok_or_else(|| match program {
        Some(p) => ExtractError::format(format!("no Program NCA {p:016x} in the base NSP")),
        None => ExtractError::format("no Program NCA in the base NSP"),
    })?;
    let up = match update {
        Some(u) => Some(
            pick(u, ContentType::Program, Some(bp.title_id)).ok_or_else(|| {
                ExtractError::format("no Program NCA of the base game's program in the update NSP")
            })?,
        ),
        None => None,
    };
    let ctrl = pick(
        update.unwrap_or(base),
        ContentType::Control,
        Some(bp.title_id),
    );
    Ok((bp, up, ctrl))
}
