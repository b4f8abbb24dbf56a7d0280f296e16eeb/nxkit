//! The rewrite of one module (`patch_nso.py`): layout, registered cuts, site analysis and rewriting, gates,
//! spin waits, output files.

use std::collections::HashSet;
use std::fmt::Write;

use crate::analyse::{Analysis, analyse};
use crate::cuts::cuts_for;
use crate::error::{RewriteError, fail};
use crate::nso::load;
pub use crate::options::Options;
use crate::options::Step;
use crate::patches::PatchTable;
use crate::rw::{R, Rw, dis};
use crate::scan::Kind;

/// The files a rewrite produces: `(file name, bytes)`.
pub struct Output {
    /// The output files (`<name>.patched.bin`, `.fixups.tsv`, `.gates.tsv`, `.sites.tsv`, `.cuts.tsv`, `.obs.tsv`,
    /// `build_src.sha256`, and `.match.tsv` for an unregistered build).
    pub files: Vec<(String, Vec<u8>)>,
    /// The lines the script prints.
    pub log: Vec<String>,
}

fn hx(v: i64) -> String {
    if v < 0 {
        format!("-{:#x}", -v)
    } else {
        format!("{v:#x}")
    }
}

/// Rewrites the NSO or NRO `nso` (file bytes).
pub fn rewrite(nso: &[u8], opts: &Options, patches: &PatchTable) -> Result<Output, RewriteError> {
    match run(nso, opts, patches, opts.patch_front) {
        Err(Rerun(need)) => match run(nso, opts, patches, Some(need)) {
            Err(Rerun(_)) => {
                fail("assertion failed: patch area does not fit after the second pass")
            }
            Ok(r) => r,
        },
        Ok(r) => r,
    }
}

struct Rerun(u64);

struct Layout {
    rw: Rw,
    build_id: String,
}

fn layout(nso: &[u8], opts: &Options, front_env: Option<u64>) -> R<Layout> {
    let m = load(nso, &opts.name)?;
    let bss = m.bss;
    let text_end = m.text_end() as i64;
    let near = ((((m.image.len() + bss + 0x3fff) & !0x3fff) + 0x4000 + 0xffff) & !0xffff) as i64;
    let far = near > 0x700_0000;
    let mut front: i64 = 0x200_0000;
    let big_text = far && text_end + front > 0x800_0000 - 0x1_0000;
    let two = big_text && text_end > 0x800_0000 - 0x1_0000;
    let env = front_env.filter(|v| *v != 0).map(|v| v as i64);
    // TWO: the lowest site the back area reaches with the margin (BACK_OFF less the reach, plus 1 MB)
    let back_min = if two {
        let b = ((((m.image.len() + bss + 0x3fff) & !0x3fff) + 0x8000 + 0xffff) & !0xffff) as i64
            - 0x800_0000
            + 0x10_0000;
        if b >= 0x800_0000 - 0x20_0000 {
            return fail(format!(
                "assertion failed: no split reaches: the back patch area reaches no site below {b:#x}, too close to the front area's reach"
            ));
        }
        b
    } else {
        0
    };
    if two {
        front =
            env.unwrap_or_else(|| 0x100_0000.min((0x800_0000 - 0x10_0000 - back_min) & !0xffff));
    } else if big_text {
        front = env.unwrap_or((0x800_0000 - 0x1_0000 - text_end) & !0xffff);
    }
    let split = if two {
        (0x800_0000 - front - 0x10_0000).max(back_min)
    } else {
        0
    };
    let mut img = m.image;
    if !far {
        img.resize(img.len() + bss, 0);
    }
    let (ro_off, data_off) = (m.segments[1].0, m.segments[2].0);
    let (key_page_off, image_size, patch_off, back_off, file_image_len);
    if far {
        file_image_len = (img.len() + 0x3fff) & !0x3fff;
        key_page_off = ((img.len() + bss + 0x3fff) & !0x3fff) as i64;
        image_size = key_page_off + 0x4000;
        patch_off = -front;
        back_off = if two {
            (key_page_off + 0x8000 + 0xffff) & !0xffff
        } else {
            0
        };
    } else {
        key_page_off = ((img.len() + 0x3fff) & !0x3fff) as i64;
        img.resize(img.len() + 0x4000, 0);
        patch_off = ((img.len() + 0xffff) & !0xffff) as i64;
        image_size = 0;
        back_off = 0;
        file_image_len = 0;
    }
    let build_id: String = m.build_id[..0x10]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let rw = Rw {
        name: opts.name.clone(),
        img,
        text_end,
        far,
        front,
        big_text,
        two,
        split,
        key_page_off,
        image_size,
        patch_off,
        back_off,
        file_image_len,
        ro_off,
        data_off,
        front_area: Vec::new(),
        back_area: Vec::new(),
        in_back: false,
        gates: Vec::new(),
        nfix: 0,
        stats: Vec::new(),
    };
    Ok(Layout { rw, build_id })
}

fn run(
    nso: &[u8],
    opts: &Options,
    patches: &PatchTable,
    front_env: Option<u64>,
) -> Result<R<Output>, Rerun> {
    match run_inner(nso, opts, patches, front_env) {
        Ok(o) => Ok(Ok(o)),
        Err(Step::Rerun(n)) => Err(Rerun(n)),
        Err(Step::Err(e)) => Ok(Err(e)),
    }
}

/// `changed_runs()`: the text words that no longer run as the file has them (changed words, windows, spin waits' own
/// instructions) as runs of (offset, length).
fn changed_runs(
    img: &[u8],
    orig: &[u8],
    windows: &[Vec<nxb_a64::Insn>],
    regions: &[crate::spin::Region],
) -> Vec<(i64, i64)> {
    let mut words = std::collections::BTreeSet::new();
    let end = orig.len();
    for (i, (a, b)) in img[..end].chunks(4).zip(orig.chunks(4)).enumerate() {
        if a != b {
            words.insert((i * 4) as i64);
        }
    }
    for w in windows {
        if let (Some(f), Some(l)) = (w.first(), w.last()) {
            let mut a = f.addr as i64;
            while a < l.addr as i64 + 4 {
                words.insert(a);
                a += 4;
            }
        }
    }
    for g in regions {
        for a in &g.r {
            if let Some(n) = g.nodes.get(a) {
                for i in 0..n.ins.len() as i64 {
                    words.insert(a + 4 * (i - n.pre as i64));
                }
            }
        }
    }
    let mut runs: Vec<(i64, i64)> = Vec::new();
    for w in words {
        match runs.last_mut() {
            Some(r) if r.0 + r.1 == w => r.1 += 4,
            _ => runs.push((w, 4)),
        }
    }
    runs
}

fn bad(m: impl Into<String>) -> Step {
    Step::Err(RewriteError::Failed(m.into()))
}

fn run_inner(
    nso: &[u8],
    opts: &Options,
    patches: &PatchTable,
    front_env: Option<u64>,
) -> Result<Output, Step> {
    let Layout { mut rw, build_id } = layout(nso, opts, front_env)?;
    let orig_text = rw.img[..rw.text_end as usize].to_vec();
    let mut log: Vec<String> = Vec::new();
    let mut extra: Vec<(String, Vec<u8>)> = Vec::new();
    let mut cuts = cuts_for(&rw, opts, patches, &build_id, &mut log, &mut extra)?;
    for c in &cuts {
        let ob = crate::branch::unhex(&c.old);
        let nb = crate::branch::unhex(&c.new);
        let cur = rw.img.get(c.off..c.off + ob.len());
        if ob.len() != nb.len() || cur != Some(&ob[..]) {
            return Err(bad(format!(
                "assertion failed: cut original bytes mismatch {:#x}",
                c.off
            )));
        }
        rw.img[c.off..c.off + nb.len()].copy_from_slice(&nb);
    }
    let ue = crate::engine::engine_finders(&mut rw, opts, &mut cuts, &mut log)?;
    rw.lock_rewrites(&mut log)?;
    let scan = rw.scan()?;
    let analysis = analyse(&rw, &scan)?;
    let Analysis {
        windows,
        retries,
        jumps,
        tabjumps,
        tailcalls,
        done,
    } = analysis;
    // rewrite the sites
    for w in &windows {
        rw.use_area(w[0].addr as i64);
        let (st, lo, _) = rw.site_window(w)?;
        rw.stat("window");
        rw.put_branch(lo, st)?;
    }
    for parts in &retries {
        let at = parts[0].0[0].addr as i64;
        rw.use_area(at);
        let st = rw.site_retry_window(parts)?;
        rw.stat("retry_window");
        rw.put_branch(at, st)?;
    }
    for (a, (k, arg)) in &scan.kinds {
        let a = *a;
        if done.contains(&a) {
            continue;
        }
        rw.use_area(a);
        let ins = dis(rw.w32(a), a)?;
        let st = match k {
            Kind::X18 => {
                let m = ins.mn;
                if m == "br" && tailcalls.contains(&a) {
                    rw.stat("br_call");
                    rw.site_br_call()?
                } else if m == "br" {
                    let st = if let Some((_, t)) = tabjumps.iter().find(|(x, _)| *x == a) {
                        rw.site_br_tab(t)?
                    } else if let Some((_, (b, c))) = jumps.iter().find(|(x, _)| *x == a) {
                        rw.site_br(*b, *c)?
                    } else {
                        return Err(bad(format!("KeyError {a:#x}")));
                    };
                    rw.stat("br");
                    st
                } else if matches!(m, "cbz" | "cbnz" | "tbz" | "tbnz") {
                    rw.stat("cond");
                    rw.site_cond(&ins)?
                } else if matches!(m, "adr" | "adrp") {
                    rw.stat("pcrel");
                    rw.site_pcrel(&ins)?
                } else if m == "blr" {
                    rw.stat("blr");
                    rw.site_blr(&ins)?
                } else if matches!(ins.reg(0), Some(nxb_a64::Reg::Sp | nxb_a64::Reg::Wsp)) {
                    rw.stat("sp");
                    rw.site_sp(&ins)?
                } else if matches!(m, "ret" | "b" | "bl") || (m == "ldr" && !ins.has_mem()) {
                    return Err(bad(format!("{a:x} {m} {}", ins.op_str())));
                } else {
                    rw.stat("plain");
                    rw.site_plain(&ins)?
                }
            }
            Kind::TpidrR => {
                rw.stat(k.name());
                rw.site_sysreg_read(&ins, crate::rw::KTLS)?
            }
            Kind::TpidrroR => {
                rw.stat(k.name());
                rw.site_sysreg_read(&ins, crate::rw::KTLR)?
            }
            Kind::TpidrW => {
                rw.stat(k.name());
                rw.site_tpidr_write(&ins)?
            }
            Kind::Svc => {
                rw.stat(k.name());
                rw.site_gate(&ins, "svc", *arg)?
            }
            Kind::Cntpct | Kind::Cntvct => {
                rw.stat(k.name());
                rw.site_gate(&ins, k.name(), *arg)?
            }
            Kind::SpAlign => {
                rw.stat(k.name());
                rw.site_sp_align(&ins, *arg as u32)?
            }
            Kind::Ctr => {
                if *arg == 18 {
                    return Err(bad(format!("assertion failed: {a:x} mrs x18, ctr_el0")));
                }
                rw.stat(k.name());
                rw.put32(a, 0xd2a0_0080 | *arg as u32);
                continue;
            }
        };
        rw.put_branch(a, st)?;
    }
    let reg_obs = patches.obs.get(&build_id).cloned().unwrap_or_default();
    rw.apply_obs(&reg_obs, &scan.kinds, &windows)?;
    let sym_obs = rw.symbol_gates(
        &scan.kinds,
        &windows,
        &mut log,
        ue.malloc.as_ref().map(|m| m.call),
        ue.region.as_ref(),
    )?;
    let (mut fs_state, mut render_scale) = (0i64, false);
    if patches.frame.contains(&build_id) && opts.name == "main" {
        let Some(ops) = patches.frame_ops.get(&build_id) else {
            return Err(Step::Err(RewriteError::Unsupported(
                "a registered FRAME_STEP stub of this main build that cannot be replayed".into(),
            )));
        };
        fs_state = rw.key_page_off + 0x4000;
        if rw.far {
            rw.image_size = fs_state + 0x4000;
        }
        if fs_state % 0x1000 != 0 || !(rw.far || fs_state + 0x40 <= rw.patch_off) {
            return Err(bad("assertion failed: frame step state page"));
        }
        render_scale = rw.replay_frame_step(ops, fs_state, &scan.kinds, &windows, &mut cuts)?;
    }
    // spin waits
    let mut touched: HashSet<i64> = scan.kinds.keys().copied().collect();
    touched.extend(done.iter().copied());
    touched.extend(reg_obs.iter().map(|o| o.off as i64));
    touched.extend(sym_obs.iter().map(|o| o.off));
    for c in &cuts {
        let mut o = c.off as i64;
        while o < c.off as i64 + (c.old.len() / 2) as i64 {
            touched.insert(o);
            o += 4;
        }
    }
    let mut regions = Vec::new();
    for (t, bes) in &scan.spin_cand {
        if let Some(g) = rw.spin_region(*t, bes, &scan.data_blocks, &touched)? {
            regions.push(g);
        }
    }
    let spin_sites: Vec<(i64, Vec<i64>)> = regions
        .iter()
        .map(|g| (g.t, g.valid.iter().copied().collect()))
        .collect();
    for g in &regions {
        rw.spin_emit(g)?;
    }
    let changed = changed_runs(&rw.img, &orig_text, &windows, &regions);
    // layout of the output
    let back_len = (rw.back_area.len() + 0x3fff) & !0x3fff;
    let mut patch = rw.front_area.clone();
    let mut image_size = rw.image_size;
    let mut data: Vec<u8>;
    if rw.far {
        if rw.big_text && patch.len() as i64 > rw.front - 0x4000 && front_env.is_none() {
            let need = (patch.len() as u64 + 0x4000 + 0xffff) & !0xffff;
            return Err(Step::Rerun(need));
        }
        if patch.len() as i64 > rw.front - 0x4000 {
            return Err(bad(format!(
                "assertion failed: patch area of {:#x} bytes does not fit in the {:#x} before the base",
                patch.len(),
                rw.front
            )));
        }
        if rw.two {
            if image_size > rw.back_off {
                return Err(bad(
                    "assertion failed: back patch area overlaps the key page or the frame step page",
                ));
            }
            image_size = rw.back_off + back_len as i64;
            let pad = (0x4000 - patch.len() % 0x4000) % 0x4000;
            patch.resize(patch.len() + pad, 0);
            patch.extend_from_slice(&rw.back_area);
            patch.resize(patch.len() + back_len - rw.back_area.len(), 0);
        }
        data = rw.img.clone();
        data.resize(rw.file_image_len, 0);
        data.extend_from_slice(&patch);
    } else {
        data = rw.img.clone();
        data.resize(rw.patch_off as usize, 0);
        data.extend_from_slice(&patch);
    }
    let n_cuts = cuts.len();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let name = &opts.name;
    files.push((format!("{name}.patched.bin"), data));
    let mut s = String::new();
    for (sha, rel) in &opts.source_hashes {
        let _ = writeln!(s, "{sha}  {rel}");
    }
    files.push(("build_src.sha256".into(), s.into_bytes()));
    let mut f = format!(
        "# PATCH_OFF\t{}\n# KEY_PAGE_OFF\t{:#x}\n# KEY_PAGE_SIZE\t0x4000\n# TEXT_END\t{:#x}\n# RO_OFF\t{:#x}\n# DATA_OFF\t{:#x}\n",
        hx(rw.patch_off),
        rw.key_page_off,
        rw.text_end,
        rw.ro_off,
        rw.data_off
    );
    if rw.far {
        let _ = write!(
            f,
            "# IMAGE_SIZE\t{image_size:#x}\n# FILE_IMAGE_LEN\t{:#x}\n",
            rw.file_image_len
        );
    }
    if rw.two {
        let _ = write!(
            f,
            "# BACK_OFF\t{:#x}\n# BACK_LEN\t{back_len:#x}\n",
            rw.back_off
        );
    }
    for (k, i) in [("K18", 0), ("KTLS", 1), ("KTLR", 2), ("KGATE", 3)] {
        let _ = writeln!(f, "# KEY_{k}_IDX\t{i}");
    }
    if fs_state != 0 {
        let _ = writeln!(f, "# FPS_STATE_OFF\t{fs_state:#x}");
    }
    if let Some(m) = &ue.malloc {
        let _ = writeln!(
            f,
            "# UE_MALLOC_VT\t{:#x}\n# UE_MALLOC_SLOT\t{:#x}",
            m.vt, m.slot
        );
    }
    if let Some((g, fld)) = ue.fps {
        let _ = writeln!(f, "# FPS_TARGET_PTR\t{g:#x}\n# FPS_TARGET_FIELD\t{fld:#x}");
    }
    for (n, o) in &ue.cvars {
        let _ = writeln!(f, "# UE_CVAR:{n}\t{o:#x}");
    }
    if render_scale {
        f.push_str("# RENDER_SCALE\t1\n");
    }
    if opts.name == "main" {
        for c in patches.caps.get(&build_id).into_iter().flatten() {
            let ok = c.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && c.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
            if !ok {
                return Err(bad(format!("assertion failed: invalid capability {c}")));
            }
            let _ = writeln!(f, "# {c}\t1");
        }
    }
    let _ = writeln!(f, "# BUILD_ID\t{build_id}");
    files.push((format!("{name}.fixups.tsv"), f.into_bytes()));
    let mut g = String::new();
    for x in &rw.gates {
        let _ = writeln!(g, "{}\t{}\t{}\t{}", hx(x.ret), x.kind, x.arg, hx(x.orig));
    }
    files.push((format!("{name}.gates.tsv"), g.into_bytes()));
    let mut s = String::new();
    for (a, (k, _)) in &scan.kinds {
        if !done.contains(a) {
            let _ = writeln!(s, "{a:#x}\t{}", k.name());
        }
    }
    for w in &windows {
        let _ = writeln!(s, "{:#x}\tW\t{:#x}", w[0].addr, w[w.len() - 1].addr + 4);
    }
    for (a, (b, c)) in &jumps {
        let _ = writeln!(s, "{a:#x}\tJ\t{b:#x}\t{c}");
    }
    for (a, tgt) in &tabjumps {
        let _ = writeln!(
            s,
            "{a:#x}\tT\t{}",
            tgt.iter()
                .map(|t| format!("{t:#x}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    for (t, bes) in &spin_sites {
        let _ = writeln!(
            s,
            "{t:#x}\tS\t{}",
            bes.iter()
                .map(|b| format!("{b:#x}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    files.push((format!("{name}.sites.tsv"), s.into_bytes()));
    let mut c = format!("# BUILD_ID\t{build_id}\n");
    for x in cuts.drain(..) {
        let _ = writeln!(c, "{:#x}\t{}\t{}\t{}", x.off, x.old, x.new, x.why);
    }
    files.push((format!("{name}.cuts.tsv"), c.into_bytes()));
    let mut o = format!("# BUILD_ID\t{build_id}\n");
    for x in &reg_obs {
        let _ = writeln!(o, "{:#x}\t{}\t{}\t{}", x.off, x.id, x.word, x.why);
    }
    for x in &sym_obs {
        let _ = writeln!(o, "{:#x}\t{}\t{}\t{}", x.off, x.id, x.word, x.why);
    }
    files.push((format!("{name}.obs.tsv"), o.into_bytes()));
    let mut ch = format!("# BUILD_ID\t{build_id}\n");
    for (off, n) in &changed {
        let _ = writeln!(ch, "{off:#x}\t{n:#x}");
    }
    files.push((format!("{name}.changed.tsv"), ch.into_bytes()));
    files.extend(extra);
    let stats: Vec<String> = rw
        .stats
        .iter()
        .map(|(k, v)| format!("'{k}': {v}"))
        .collect();
    log.push(format!(
        "{name} {{{}}} cuts {} obs {} patch {} PATCH_OFF {} slot_refs {} gates {}",
        stats.join(", "),
        n_cuts,
        reg_obs.len() + sym_obs.len(),
        rw.front_area.len() + rw.back_area.len(),
        hx(rw.patch_off),
        rw.nfix,
        rw.gates.len()
    ));
    if !scan.data_blocks.is_empty() {
        let blocks: Vec<String> = scan
            .data_blocks
            .iter()
            .map(|(s, e)| format!("{s:#x}..{e:#x}"))
            .collect();
        log.push(format!(
            "{name} data blocks left alone: {}",
            blocks.join(", ")
        ));
    }
    Ok(Output { files, log })
}
