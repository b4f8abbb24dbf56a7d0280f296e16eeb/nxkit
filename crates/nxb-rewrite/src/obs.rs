//! Observation gates: the registered ones, the address region gates found by the SDK's own symbols, and the
//! `StandardAllocator::Initialize` gate.

use crate::dynsym::defined;

use nxb_a64::{Insn, Op, Reg, decode};

use crate::error::{RewriteError, fail};
use crate::pat::{memop, nops, preg};
use crate::rw::{R, Rw};
use crate::scan::Kind;
use std::collections::BTreeMap;

/// The one store through a function's out pointer (`mov xB, x1` once in the prologue, then `str xA, [xB]`).
fn out_store(img: &[u8], start: i64, size: i64) -> Result<(i64, u32), String> {
    let mut ins: Vec<Insn> = Vec::new();
    let mut a = start;
    while a < start + size {
        let Some(w) = img.get(a as usize..a as usize + 4) else {
            break;
        };
        match decode(u32::from_le_bytes([w[0], w[1], w[2], w[3]]), a as u64) {
            Some(i) => ins.push(i),
            None => break,
        }
        a += 4;
    }
    let out: Vec<&Insn> = ins
        .iter()
        .take(16)
        .filter(|i| {
            i.mn == "mov"
                && nops(i, 2)
                && matches!(preg(i, 0), Some(Reg::X(_)))
                && preg(i, 1) == Some(Reg::X(1))
        })
        .collect();
    if out.len() != 1 {
        return Err("no single copy of the out pointer in the prologue".into());
    }
    let xb = preg(out[0], 0).unwrap_or(Reg::X(0));
    let st: Vec<&Insn> = ins
        .iter()
        .filter(|i| {
            i.mn == "str"
                && nops(i, 2)
                && matches!(preg(i, 0), Some(Reg::X(_)))
                && memop(i, 1).is_some_and(|m| {
                    m.base == xb && m.index.is_none() && m.disp == 0 && m.wb == nxb_a64::Wb::None
                })
        })
        .collect();
    if st.len() != 1 {
        return Err(format!("{} stores through the out pointer", st.len()));
    }
    let reg = match preg(st[0], 0) {
        Some(Reg::X(n)) => u32::from(n),
        _ => 0,
    };
    Ok((st[0].addr as i64, reg))
}

/// A registered or found observation gate.
pub(crate) struct ObsRow {
    pub(crate) off: i64,
    pub(crate) id: u64,
    pub(crate) word: String,
    pub(crate) why: String,
}

const OBS_REGION_ASK: u64 = 22;
const OBS_REGION_GOT: u64 = 23;
const OBS_REGION_FREE: u64 = 24;
const OBS_STDALLOC_INIT: u64 = 25;
const OBS_UE_REGION: u64 = 26;

impl Rw {
    fn in_window(windows: &[Vec<Insn>], off: i64) -> bool {
        windows
            .iter()
            .any(|w| (w[0].addr as i64) <= off && off < w[w.len() - 1].addr as i64 + 4)
    }

    /// The registered observation gates of the build.
    pub(crate) fn apply_obs(
        &mut self,
        obs: &[crate::patches::Obs],
        kinds: &BTreeMap<i64, (Kind, i64)>,
        windows: &[Vec<Insn>],
    ) -> R<()> {
        for o in obs {
            let off = o.off as i64;
            let w = self.w32(off);
            if format!("{w:08x}") != o.word {
                return fail(format!(
                    "assertion failed: observation gate original instruction mismatch {off:#x}: {w:08x}"
                ));
            }
            // any instruction `site_obs` can relocate: the gate frame saves x0 to x28 and the flags and restores them
            if !(off < self.text_end && !kinds.contains_key(&off) && !Self::in_window(windows, off))
            {
                return fail(format!(
                    "assertion failed: observation gate conflicts with a rewrite site {off:#x}"
                ));
            }
            self.use_area(off);
            let st = self.site_obs(off, o.id, w)?;
            self.put_branch(off, st)?;
            self.stat("obs");
        }
        Ok(())
    }

    /// The address region gates and the `StandardAllocator::Initialize` gates found by symbol.
    pub(crate) fn symbol_gates(
        &mut self,
        kinds: &BTreeMap<i64, (Kind, i64)>,
        windows: &[Vec<Insn>],
        log: &mut Vec<String>,
        ue_call: Option<usize>,
        region: Option<&crate::region::UeRegion>,
    ) -> R<Vec<ObsRow>> {
        let sym = defined(&self.img);
        let mut rows: Vec<ObsRow> = Vec::new();
        let ask = sym
            .get("_ZN2nn2os6detail11VammManager21AllocateAddressRegionEPmm")
            .copied();
        let free = sym
            .get("_ZN2nn2os6detail11VammManager17FreeAddressRegionEm")
            .copied();
        let name = self.name.clone();
        if let (Some(ask), Some(free)) = (ask, free) {
            match out_store(&self.img, ask.0, ask.1) {
                Err(e) => log.push(format!("{name}: no address region gates: {e}")),
                Ok((got, reg)) => {
                    let sites = [
                        (
                            ask.0,
                            OBS_REGION_ASK,
                            "VammManager::AllocateAddressRegion entry: the size in x2".to_string(),
                        ),
                        (
                            got,
                            OBS_REGION_GOT | u64::from(reg) << 8,
                            format!(
                                "VammManager::AllocateAddressRegion: the new region's address in x{reg}, stored through the out pointer"
                            ),
                        ),
                        (
                            free.0,
                            OBS_REGION_FREE,
                            "VammManager::FreeAddressRegion entry: the address in x1".to_string(),
                        ),
                    ];
                    let mut bad = None;
                    for (off, _, _) in &sites {
                        if *off >= self.text_end
                            || kinds.contains_key(off)
                            || Self::in_window(windows, *off)
                        {
                            bad = Some(format!("no observation gate possible at {off:#x}"));
                            break;
                        }
                    }
                    match bad {
                        Some(b) => log.push(format!("{name}: no address region gates: {b}")),
                        None => {
                            for (off, id, why) in sites {
                                rows.push(ObsRow {
                                    off,
                                    id,
                                    word: format!("{:08x}", self.w32(off)),
                                    why,
                                });
                            }
                        }
                    }
                }
            }
        }
        for (n, sfx) in [
            ("_ZN2nn3mem17StandardAllocator10InitializeEPvm", ""),
            ("_ZN2nn3mem17StandardAllocator10InitializeEPvmb", ", bool"),
        ] {
            let Some(s) = sym.get(n) else { continue };
            let off = s.0;
            if off >= self.text_end || kinds.contains_key(&off) || Self::in_window(windows, off) {
                log.push(format!(
                    "{name}: no StandardAllocator gate at {off:#x}: a rewrite site"
                ));
                continue;
            }
            rows.push(ObsRow {
                off,
                id: OBS_STDALLOC_INIT,
                word: format!("{:08x}", self.w32(off)),
                why: format!("StandardAllocator::Initialize(void *, size_t{sfx}) entry: the size in x2, capped when the address in x1 is null"),
            });
        }
        if let Some(c) = ue_call {
            let c = c as i64;
            if c >= self.text_end || kinds.contains_key(&c) || Self::in_window(windows, c) {
                return fail(format!(
                    "assertion failed: FMallocBinned3 gate conflicts with a rewrite site {c:#x}"
                ));
            }
            rows.push(ObsRow {
                off: c,
                id: 21,
                word: "d503201f".into(),
                why: "FMallocBinned3 constructor call (cut): the object in x0; the host puts its allocator's vtable in (hle_uemalloc.c)".into(),
            });
        }
        if let Some(r) = region {
            let (off, reg) = (r.off as i64, r.reg);
            if kinds.contains_key(&off) || Self::in_window(windows, off) {
                return fail(format!(
                    "assertion failed: engine region gate conflicts with a rewrite site {off:#x}"
                ));
            }
            rows.push(ObsRow {
                off,
                id: OBS_UE_REGION | u64::from(reg) << 8,
                word: format!("{:08x}", self.w32(off)),
                why: format!("Unreal's engine region: its size in x{reg}, stored before nninitStartup reserves it; the host gives what the alias region has room for (hle_svc.c)"),
            });
            if let Some(x) = r.x1 {
                let x = x as i64;
                if kinds.contains_key(&x) || Self::in_window(windows, x) {
                    return fail(format!(
                        "assertion failed: engine region size conflicts with a rewrite site {x:#x}"
                    ));
                }
                self.put32(x, 0xAA00_03E1 | u32::from(reg) << 16);
            }
        }
        for r in &rows {
            let w =
                u32::from_str_radix(&r.word, 16).map_err(|_| RewriteError::Failed("hex".into()))?;
            self.use_area(r.off);
            let st = self.site_obs(r.off, r.id, w)?;
            self.put_branch(r.off, st)?;
            self.stat("obs");
        }
        Ok(rows)
    }
}

/// Unused marker so the operand type stays referenced.
#[allow(dead_code)]
fn _o(_: Op) {}
