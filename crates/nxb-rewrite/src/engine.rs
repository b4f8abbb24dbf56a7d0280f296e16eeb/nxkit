//! The engine finders of `patch_nso.py` that run on any module without a build ID, wired into the rewrite
//! (`ue_cvar`, `fps_target`, `ue_malloc`, `snowdrop_pools`, `ue_region`, `ue_ring`).

use crate::error::RewriteError;
use crate::options::{Options, Step};
use crate::rw::Rw;

/// What the engine finders left for the fixups table.
pub(crate) struct Finds {
    pub(crate) cvars: Vec<(String, i64)>,
    pub(crate) fps: Option<(i64, i64)>,
    pub(crate) malloc: Option<crate::finders2::UeMalloc>,
    pub(crate) region: Option<crate::region::UeRegion>,
}

/// The finders that run on any module without a build ID (`ue_cvar`, `fps_target`, `ue_malloc`, `snowdrop_pools`,
/// `ue_region`, `ue_ring`): their cuts go on the cut list and into the image, in the order of the script.
pub(crate) fn engine_finders(
    rw: &mut Rw,
    opts: &Options,
    cuts: &mut Vec<crate::patches::Cut>,
    log: &mut Vec<String>,
) -> Result<Finds, Step> {
    let name = &opts.name;
    let fail_ = |m: String| Step::Err(RewriteError::Failed(m));
    let (text_end, ro_off, data_off) = (rw.text_end as usize, rw.ro_off, rw.data_off);
    let mut f = Finds {
        cvars: Vec::new(),
        fps: None,
        malloc: None,
        region: None,
    };
    let main = name == "main";
    if main {
        let (found, why) = crate::finders::cvar_find_all(&rw.img, text_end, ro_off, data_off);
        if !found.is_empty() {
            let l: Vec<String> = found
                .iter()
                .map(|(n, o)| format!("{n} at {o:#x}"))
                .collect();
            log.push(format!(
                "{name}: Unreal console variables (ue_cvar.py): {}",
                l.join(", ")
            ));
        }
        for w in &why {
            if let Some(r) = w.strip_prefix("[RED] ") {
                log.push(format!(
                    "[RED] {name}: Unreal console variable {r}: the host cannot write it"
                ));
            }
        }
        f.cvars = found;
        let img = rw.img.clone();
        let (g, fld, why) = crate::finders::fps_find(&img, text_end, data_off, &|| {
            crate::finders2::got_values(&img)
        })
        .map_err(fail_)?;
        if let (Some(g), Some(fld)) = (g, fld) {
            log.push(format!("{name}: {why} (fps_target.py)"));
            f.fps = Some((g, fld));
        } else if let Some(r) = why.strip_prefix("[RED] ") {
            log.push(format!("[RED] {name}: {r}: the host cannot write it"));
        }
    }
    match crate::finders2::ue_malloc_find(&rw.img, text_end, ro_off, data_off).map_err(fail_)? {
        Ok(m) => {
            let old: String = rw.img[m.call..m.call + 4]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            cuts.push(crate::patches::Cut {
                off: m.call,
                old,
                new: "1f2003d5".into(),
                why: format!(
                    "{}: the constructor call → nop, gate OBS_UE_MALLOC on it (ue_malloc.py)",
                    m.why
                ),
            });
            rw.img[m.call..m.call + 4].copy_from_slice(&0xD503_201Fu32.to_le_bytes());
            log.push(format!("{name}: {}", m.why));
            f.malloc = Some(m);
        }
        Err(why) => {
            if let Some(r) = why.strip_prefix("[RED] ") {
                log.push(format!("[RED] {name}: {r}: Unreal's own allocator runs"));
            }
        }
    }
    if main {
        let (sites, why) = crate::snowdrop::snowdrop_find(&rw.img, text_end);
        if let Some(s) = sites {
            let sizes = if opts.snowdrop_words.is_empty() {
                crate::snowdrop::SNOWDROP_DEFAULT
            } else {
                parse_pools(&opts.snowdrop_words).map_err(fail_)?
            };
            let from = if opts.snowdrop_words.is_empty() {
                if opts.snowdrop_from.is_empty() {
                    "default, no DM_GAME"
                } else {
                    opts.snowdrop_from.as_str()
                }
            } else {
                "profile"
            };
            for (off, old, new, why) in crate::snowdrop::snowdrop_cuts(&rw.img, &s, sizes) {
                let nb = crate::branch::unhex(&new);
                rw.img[off..off + 4].copy_from_slice(&nb);
                cuts.push(crate::patches::Cut { off, old, new, why });
            }
            log.push(format!(
                "{name}: {why}: large {} MB, 512 KB pages {} MB, 64 KB pages {} MB ({from})",
                sizes[0], sizes[1], sizes[2]
            ));
        } else if let Some(r) = why.strip_prefix("[RED] ") {
            log.push(format!(
                "[RED] {name}: {r}: its 12 GB reservation stays and fails on an iPhone"
            ));
        }
        match crate::region::ue_region_find(&rw.img, text_end, ro_off, data_off).map_err(fail_)? {
            Ok(r) => {
                log.push(format!("{name}: {}", r.why));
                f.region = Some(r);
            }
            Err(why) => {
                if let Some(r) = why.strip_prefix("[RED] ") {
                    log.push(format!(
                        "[RED] {name}: {r}: the engine asks for its whole default region"
                    ));
                }
            }
        }
        match crate::ring::ue_ring_find(&rw.img, text_end, ro_off, data_off).map_err(fail_)? {
            Ok(r) => {
                let hex = |w: u32| -> String {
                    w.to_le_bytes().iter().map(|b| format!("{b:02x}")).collect()
                };
                cuts.push(crate::patches::Cut {
                    off: r.off,
                    old: hex(r.old),
                    new: hex(r.new),
                    why: format!("{} (ue_ring.py)", r.why),
                });
                rw.img[r.off..r.off + 4].copy_from_slice(&r.new.to_le_bytes());
                log.push(format!("{name}: {}", r.why));
            }
            Err(why) => {
                if let Some(r) = why.strip_prefix("[RED] ") {
                    log.push(format!(
                        "[RED] {name}: {r}: its dynamic buffers keep 3 copies"
                    ));
                }
            }
        }
    }
    Ok(f)
}

/// `snowdrop_pools.parse`: the three sizes in MB from a profile's words.
fn parse_pools(words: &[String]) -> Result<[u64; 3], String> {
    let bad = || {
        format!(
            "[RED] profile: snowdrop_pools takes the large, 512 KB and 64 KB ranges in MB, under 4 GB in all: {}",
            words.join(" ")
        )
    };
    let v: Vec<u64> = words
        .iter()
        .map(|x| {
            x.strip_prefix("0x")
                .map_or_else(|| x.parse().ok(), |h| u64::from_str_radix(h, 16).ok())
        })
        .collect::<Option<_>>()
        .ok_or_else(bad)?;
    if v.len() != 3 || v.contains(&0) || v.iter().sum::<u64>() >= 4096 {
        return Err(bad());
    }
    Ok([v[0], v[1], v[2]])
}
